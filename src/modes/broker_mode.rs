use bytes::Bytes;
use std::error::Error;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use zeromq::{PubSocket, RouterSocket, Socket, SocketRecv, SocketSend, ZmqMessage};

use super::console::{
    format_conn_count, format_console_help, format_unknown_command, format_uptime,
    parse_console_command, ConnCounter, ConsoleCommand, STDIN_EOF_NOTICE,
};
use super::metrics::{spawn_metrics_server, MetricsState};
use super::pub_mode::TwoPartSender;
use super::router_mode::build_router_message;
use super::sub_mode::bytes_to_display;

/// Classification of an incoming Router message in Broker mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BrokerAction {
    /// `_R`: Register worker identity into `WorkerRegistry`.
    Register(Bytes),
    /// `_U`: Unregister worker identity from `WorkerRegistry`.
    Unregister(Bytes),
    /// Any other `_`-prefixed topic: broadcast `[worker_id, topic, payload]` to all registered workers.
    Command {
        id: Bytes,
        topic: Bytes,
        payload: Bytes,
    },
    /// Non-`_` topic: publish `[topic, payload]` to subscribers via the PUB socket.
    Publish {
        id: Bytes,
        topic: Bytes,
        payload: Bytes,
    },
    /// Malformed message (frame count != 3 or empty identity): drop.
    Drop,
}

/// Classifies a raw `ZmqMessage` received on the Broker's ROUTER socket:
/// - Must have exactly 3 frames `[identity, topic, payload]` with non-empty `identity`, else `Drop`.
/// - Topic `b"_R"` => `Register(identity)`.
/// - Topic `b"_U"` => `Unregister(identity)`.
/// - Any other topic starting with `b'_'` => `Command { id, topic, payload }`.
/// - All other topics (including empty `""`) => `Publish { id, topic, payload }`.
pub(crate) fn classify_router_message(msg: &ZmqMessage) -> BrokerAction {
    if msg.len() != 3 {
        return BrokerAction::Drop;
    }
    let id = msg.get(0).unwrap().clone();
    if id.is_empty() {
        return BrokerAction::Drop;
    }
    let topic = msg.get(1).unwrap().clone();
    let payload = msg.get(2).unwrap().clone();

    if topic.as_ref() == b"_R" {
        BrokerAction::Register(id)
    } else if topic.as_ref() == b"_U" {
        BrokerAction::Unregister(id)
    } else if topic.first() == Some(&b'_') {
        BrokerAction::Command { id, topic, payload }
    } else {
        BrokerAction::Publish { id, topic, payload }
    }
}

/// Maintains the set of registered worker identities in deterministic registration order.
#[derive(Default, Debug, Clone)]
pub(crate) struct WorkerRegistry {
    workers: Vec<Bytes>,
}

impl WorkerRegistry {
    /// Registers a worker identity if not already present (idempotent).
    pub(crate) fn register(&mut self, id: Bytes) {
        if !self.workers.iter().any(|w| w == &id) {
            self.workers.push(id);
        }
    }

    /// Removes a worker identity if present (no-op if unknown).
    pub(crate) fn unregister(&mut self, id: &Bytes) {
        self.workers.retain(|w| w != id);
    }

    /// Returns the slice of currently registered worker identities.
    pub(crate) fn ids(&self) -> &[Bytes] {
        &self.workers
    }

    /// Returns the number of currently registered workers.
    pub(crate) fn len(&self) -> usize {
        self.workers.len()
    }
}

/// Builds 3-frame Router messages `[worker_id, topic, payload]` for every registered worker.
pub(crate) fn build_command_messages(
    reg: &WorkerRegistry,
    topic: &Bytes,
    payload: &Bytes,
) -> Vec<ZmqMessage> {
    reg.ids()
        .iter()
        .map(|worker_id| build_router_message(worker_id.clone(), topic, payload.clone()))
        .collect()
}

/// Runtime counters for Broker mode.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrokerStats {
    pub(crate) published: usize,
    pub(crate) commands: usize,
    pub(crate) registered: usize,
    pub(crate) unregistered: usize,
    pub(crate) dropped: usize,
}

/// Formats a published business message for `--debug`:
/// `[DATA] [{seq}] {id} {topic} => {payload}`
pub(crate) fn format_broker_debug_data(
    seq: usize,
    id: &str,
    topic: &str,
    payload: &str,
) -> String {
    format!("[DATA] [{}] {} {} => {}", seq, id, topic, payload)
}

/// Formats a command broadcast for `--debug`:
/// `[CMD] [{seq}] {id} {topic} => {payload} (to {n} worker(s))`
pub(crate) fn format_broker_debug_cmd(
    seq: usize,
    id: &str,
    topic: &str,
    payload: &str,
    worker_count: usize,
) -> String {
    format!(
        "[CMD] [{}] {} {} => {} (to {} worker(s))",
        seq, id, topic, payload, worker_count
    )
}

/// Formats a worker registration/unregistration event for `--debug`:
/// `[REG] register '{id}'` or `[REG] unregister '{id}'`
pub(crate) fn format_broker_debug_reg(is_register: bool, id: &str) -> String {
    let action = if is_register { "register" } else { "unregister" };
    format!("[REG] {} '{}'", action, id)
}

/// Formats the summary line printed when broker exits:
/// `Broker summary: published {p} message(s), broadcast {c} command(s), {r} registered, {u} unregistered, {d} dropped.`
pub(crate) fn format_broker_summary(stats: &BrokerStats) -> String {
    format!(
        "Broker summary: published {} message(s), broadcast {} command(s), {} registered, {} unregistered, {} dropped.",
        stats.published, stats.commands, stats.registered, stats.unregistered, stats.dropped
    )
}

/// Formats the `/s` interactive status output (including startup settings) for Broker mode.
pub(crate) fn format_broker_status(
    args: &crate::ZmqArgs,
    uptime: Duration,
    stats: &BrokerStats,
    registry: &WorkerRegistry,
    router_conns: Option<usize>,
    pub_conns: Option<usize>,
) -> String {
    let ids = registry.ids();
    let workers_str = if ids.is_empty() {
        "-".to_string()
    } else {
        ids.iter()
            .map(|id| bytes_to_display(id))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let count_label = format!("workers ({})", ids.len());
    let relay_ep = args.relay_endpoint.as_deref().unwrap_or("");
    let mon = if args.monitor { "on " } else { "off" };
    let dbg = if args.debug { "on" } else { "off" };
    let metrics_line = match args.metrics_addr {
        Some(addr) => format!("\n  metrics: http://{}/metrics", addr),
        None => String::new(),
    };
    format!(
        "[STATUS] broker | uptime {}\n  ROUTER : bind {}\n  PUB    : bind {}\n  monitor: {}   debug: {}{}\n  published               : {}\n  commands broadcast      : {}\n  registered / unregistered / dropped : {} / {} / {}\n  {:<23} : {}\n  ROUTER connections      : {}\n  PUB connections         : {}",
        format_uptime(uptime),
        args.endpoint,
        relay_ep,
        mon,
        dbg,
        metrics_line,
        stats.published,
        stats.commands,
        stats.registered,
        stats.unregistered,
        stats.dropped,
        count_label,
        workers_str,
        format_conn_count(router_conns),
        format_conn_count(pub_conns)
    )
}

/// Checks and warns about parameters that are ignored in broker mode.
pub(crate) fn check_broker_warnings(args: &crate::ZmqArgs) {
    if args.topic != "*" {
        eprintln!(
            "Warning: --topic is ignored in broker mode (topics are inspected per message)."
        );
    }
    if args.file.is_some() {
        eprintln!("Warning: --file is ignored in broker mode.");
    }
}

/// Processes a single incoming Router message according to Broker protocol rules.
pub(crate) async fn process_broker_message<P: TwoPartSender>(
    msg: &ZmqMessage,
    router: &mut RouterSocket,
    pub_sender: &mut P,
    registry: &mut WorkerRegistry,
    stats: &mut BrokerStats,
    debug: bool,
) -> Result<(), Box<dyn Error>> {
    match classify_router_message(msg) {
        BrokerAction::Register(id) => {
            registry.register(id.clone());
            stats.registered += 1;
            if debug {
                println!("{}", format_broker_debug_reg(true, &bytes_to_display(&id)));
            }
        }
        BrokerAction::Unregister(id) => {
            registry.unregister(&id);
            stats.unregistered += 1;
            if debug {
                println!("{}", format_broker_debug_reg(false, &bytes_to_display(&id)));
            }
        }
        BrokerAction::Command { id, topic, payload } => {
            stats.commands += 1;
            let out_msgs = build_command_messages(registry, &topic, &payload);
            if debug {
                println!(
                    "{}",
                    format_broker_debug_cmd(
                        stats.commands,
                        &bytes_to_display(&id),
                        &bytes_to_display(&topic),
                        &bytes_to_display(&payload),
                        out_msgs.len()
                    )
                );
            }
            for out_msg in out_msgs {
                // Disconnected or unreachable workers are silently ignored
                let _ = router.send(out_msg).await;
            }
        }
        BrokerAction::Publish { id, topic, payload } => {
            stats.published += 1;
            if debug {
                println!(
                    "{}",
                    format_broker_debug_data(
                        stats.published,
                        &bytes_to_display(&id),
                        &bytes_to_display(&topic),
                        &bytes_to_display(&payload)
                    )
                );
            }
            pub_sender.send_two_part(&topic, payload).await?;
        }
        BrokerAction::Drop => {
            stats.dropped += 1;
        }
    }
    Ok(())
}

/// Runs the Broker mode loop:
/// - ROUTER socket always binds to `--endpoint` (receives from Dealers / Workers).
/// - PUB socket always binds to `--relay-endpoint` (publishes business messages to Subscribers).
pub async fn run_broker(args: crate::ZmqArgs) -> Result<(), Box<dyn Error>> {
    check_broker_warnings(&args);

    let relay_endpoint = args
        .relay_endpoint
        .as_deref()
        .ok_or("--relay-endpoint is required in broker mode")?;

    let mut router = RouterSocket::new();
    let mut pub_sock = PubSocket::new();

    let (router_counter, _router_monitor_handle) = if args.monitor {
        let c: ConnCounter = Arc::new(AtomicUsize::new(0));
        let h = super::monitor::spawn_socket_monitor_tracked(
            router.monitor(),
            Some("[ROUTER]"),
            args.debug,
            Some(c.clone()),
        );
        (Some(c), Some(h))
    } else {
        (None, None)
    };

    let (pub_counter, _pub_monitor_handle) = if args.monitor {
        let c: ConnCounter = Arc::new(AtomicUsize::new(0));
        let h = super::monitor::spawn_socket_monitor_tracked(
            pub_sock.monitor(),
            Some("[PUB]"),
            args.debug,
            Some(c.clone()),
        );
        (Some(c), Some(h))
    } else {
        (None, None)
    };

    // Bind metrics listener before ZMQ sockets so port conflicts fail fast
    let (metrics_state, metrics_handle) = if let Some(m_addr) = args.metrics_addr {
        let mut conns = Vec::with_capacity(2);
        if let Some(c) = &router_counter {
            conns.push(("router", c.clone()));
        }
        if let Some(c) = &pub_counter {
            conns.push(("pub", c.clone()));
        }
        let state = MetricsState::new(crate::Mode::Broker, conns);
        let (bound_addr, handle) = spawn_metrics_server(m_addr, state.clone()).await?;
        println!("Metrics listening on http://{}/metrics", bound_addr);
        (Some(state), Some(handle))
    } else {
        (None, None)
    };

    router.bind(&args.endpoint).await?;
    println!("ROUTER bound to {}", args.endpoint);

    pub_sock.bind(relay_endpoint).await?;
    println!("PUB bound to {}", relay_endpoint);

    println!("Broker running (/h for commands, Ctrl+C to exit)...");

    let start_time = Instant::now();
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdin_active = true;
    let mut registry = WorkerRegistry::default();
    let mut stats = BrokerStats::default();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nShutdown requested, exiting broker.");
                println!("{}", format_broker_summary(&stats));
                break;
            }
            line_res = async {
                if stdin_active {
                    reader.next_line().await
                } else {
                    std::future::pending().await
                }
            } => {
                match line_res? {
                    Some(line) => match parse_console_command(&line) {
                        ConsoleCommand::Status => {
                            let r_c = router_counter.as_ref().map(|c| c.load(Ordering::Relaxed));
                            let p_c = pub_counter.as_ref().map(|c| c.load(Ordering::Relaxed));
                            println!(
                                "{}",
                                format_broker_status(
                                    &args,
                                    start_time.elapsed(),
                                    &stats,
                                    &registry,
                                    r_c,
                                    p_c,
                                )
                            );
                        }
                        ConsoleCommand::Help => {
                            println!("{}", format_console_help());
                        }
                        ConsoleCommand::Quit => {
                            println!("Shutdown requested, exiting broker.");
                            println!("{}", format_broker_summary(&stats));
                            break;
                        }
                        ConsoleCommand::Unknown(cmd) => {
                            println!("{}", format_unknown_command(&cmd));
                        }
                    },
                    None => {
                        stdin_active = false;
                        println!("{}", STDIN_EOF_NOTICE);
                    }
                }
            }
            recv_res = router.recv() => {
                let msg = recv_res?;
                process_broker_message(
                    &msg,
                    &mut router,
                    &mut pub_sock,
                    &mut registry,
                    &mut stats,
                    args.debug,
                ).await?;
                if let Some(m) = &metrics_state {
                    m.sync_broker(&stats, registry.len());
                }
            }
        }
    }

    if let Some(h) = metrics_handle {
        h.abort();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_3frame(id: &[u8], topic: &[u8], payload: &[u8]) -> ZmqMessage {
        let mut m = ZmqMessage::from(Bytes::copy_from_slice(id));
        m.push_back(Bytes::copy_from_slice(topic));
        m.push_back(Bytes::copy_from_slice(payload));
        m
    }

    // AC-BRK-01: _R registers, _U unregisters, _RX or _ alone is a Command
    #[test]
    fn ac_brk_01_classify_register_and_unregister() {
        let reg = make_3frame(b"w1", b"_R", b"");
        assert_eq!(
            classify_router_message(&reg),
            BrokerAction::Register(Bytes::from_static(b"w1"))
        );

        let unreg = make_3frame(b"w1", b"_U", b"ignored");
        assert_eq!(
            classify_router_message(&unreg),
            BrokerAction::Unregister(Bytes::from_static(b"w1"))
        );

        // _RX is not _R => Command
        let rx = make_3frame(b"w1", b"_RX", b"data");
        assert_eq!(
            classify_router_message(&rx),
            BrokerAction::Command {
                id: Bytes::from_static(b"w1"),
                topic: Bytes::from_static(b"_RX"),
                payload: Bytes::from_static(b"data"),
            }
        );

        // Single underscore "_" => Command
        let single_u = make_3frame(b"w1", b"_", b"ping");
        assert_eq!(
            classify_router_message(&single_u),
            BrokerAction::Command {
                id: Bytes::from_static(b"w1"),
                topic: Bytes::from_static(b"_"),
                payload: Bytes::from_static(b"ping"),
            }
        );
    }

    // AC-BRK-02: _-prefixed topic => Command; non-_ topic (including empty) => Publish
    #[test]
    fn ac_brk_02_classify_command_vs_publish() {
        let cmd = make_3frame(b"ctrl", b"_CMD_RELOAD", b"now");
        assert_eq!(
            classify_router_message(&cmd),
            BrokerAction::Command {
                id: Bytes::from_static(b"ctrl"),
                topic: Bytes::from_static(b"_CMD_RELOAD"),
                payload: Bytes::from_static(b"now"),
            }
        );

        let pub_msg = make_3frame(b"prod1", b"quote.tw", b"100.5");
        assert_eq!(
            classify_router_message(&pub_msg),
            BrokerAction::Publish {
                id: Bytes::from_static(b"prod1"),
                topic: Bytes::from_static(b"quote.tw"),
                payload: Bytes::from_static(b"100.5"),
            }
        );

        let empty_topic = make_3frame(b"prod1", b"", b"raw");
        assert_eq!(
            classify_router_message(&empty_topic),
            BrokerAction::Publish {
                id: Bytes::from_static(b"prod1"),
                topic: Bytes::new(),
                payload: Bytes::from_static(b"raw"),
            }
        );
    }

    // AC-BRK-03: Frame count != 3 or empty identity => Drop
    #[test]
    fn ac_brk_03_classify_malformed_frames_dropped() {
        // 1 frame
        let m1 = ZmqMessage::from(Bytes::from_static(b"only_id"));
        assert_eq!(classify_router_message(&m1), BrokerAction::Drop);

        // 2 frames
        let mut m2 = ZmqMessage::from(Bytes::from_static(b"id"));
        m2.push_back(Bytes::from_static(b"payload"));
        assert_eq!(classify_router_message(&m2), BrokerAction::Drop);

        // 4 frames
        let mut m4 = make_3frame(b"id", b"topic", b"payload");
        m4.push_back(Bytes::from_static(b"extra"));
        assert_eq!(classify_router_message(&m4), BrokerAction::Drop);

        // Empty identity in 3-frame message
        let empty_id = make_3frame(b"", b"topic", b"payload");
        assert_eq!(classify_router_message(&empty_id), BrokerAction::Drop);
    }

    // AC-BRK-04: WorkerRegistry is idempotent and handles unregister cleanly
    #[test]
    fn ac_brk_04_worker_registry_idempotent_and_unregister() {
        let mut reg = WorkerRegistry::default();
        assert_eq!(reg.len(), 0);

        let w1 = Bytes::from_static(b"worker-1");
        let w2 = Bytes::from_static(b"worker-2");

        reg.register(w1.clone());
        reg.register(w2.clone());
        // Duplicate registration must not add a second entry or reorder
        reg.register(w1.clone());
        assert_eq!(reg.len(), 2);
        assert_eq!(reg.ids(), &[w1.clone(), w2.clone()]);

        // Unregister unknown id is a no-op
        reg.unregister(&Bytes::from_static(b"unknown"));
        assert_eq!(reg.len(), 2);

        // Unregister w1 leaves w2
        reg.unregister(&w1);
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.ids(), &[w2]);
    }

    // AC-BRK-05: build_command_messages builds 3-frame messages for all registered workers
    #[test]
    fn ac_brk_05_build_command_messages() {
        let mut reg = WorkerRegistry::default();
        let topic = Bytes::from_static(b"_STOP");
        let payload = Bytes::from_static(b"graceful");

        // Empty registry => no messages
        assert!(build_command_messages(&reg, &topic, &payload).is_empty());

        reg.register(Bytes::from_static(b"w1"));
        reg.register(Bytes::from_static(b"w2"));

        let msgs = build_command_messages(&reg, &topic, &payload);
        assert_eq!(msgs.len(), 2);

        assert_eq!(msgs[0].len(), 3);
        assert_eq!(msgs[0].get(0).unwrap().as_ref(), b"w1");
        assert_eq!(msgs[0].get(1).unwrap().as_ref(), b"_STOP");
        assert_eq!(msgs[0].get(2).unwrap().as_ref(), b"graceful");

        assert_eq!(msgs[1].len(), 3);
        assert_eq!(msgs[1].get(0).unwrap().as_ref(), b"w2");
        assert_eq!(msgs[1].get(1).unwrap().as_ref(), b"_STOP");
        assert_eq!(msgs[1].get(2).unwrap().as_ref(), b"graceful");
    }

    // AC-BRK-06: Debug and exit summary formatting
    #[test]
    fn ac_brk_06_format_debug_and_summary() {
        assert_eq!(
            format_broker_debug_data(1, "prod1", "sensor", "42.0"),
            "[DATA] [1] prod1 sensor => 42.0"
        );
        assert_eq!(
            format_broker_debug_cmd(2, "ctrl", "_CMD", "reload", 3),
            "[CMD] [2] ctrl _CMD => reload (to 3 worker(s))"
        );
        assert_eq!(
            format_broker_debug_reg(true, "worker-1"),
            "[REG] register 'worker-1'"
        );
        assert_eq!(
            format_broker_debug_reg(false, "worker-1"),
            "[REG] unregister 'worker-1'"
        );

        let stats = BrokerStats {
            published: 10,
            commands: 2,
            registered: 3,
            unregistered: 1,
            dropped: 4,
        };
        assert_eq!(
            format_broker_summary(&stats),
            "Broker summary: published 10 message(s), broadcast 2 command(s), 3 registered, 1 unregistered, 4 dropped."
        );
    }

    // AC-BRK-08: format_broker_status with workers, empty workers, settings and connection counts
    #[test]
    fn ac_brk_08_format_broker_status() {
        use clap::Parser;
        let args = crate::ZmqArgs::try_parse_from([
            "zmqc",
            "--mode",
            "broker",
            "--endpoint",
            "tcp://127.0.0.1:55550",
            "--relay-endpoint",
            "tcp://127.0.0.1:55555",
            "--monitor",
        ])
        .unwrap();

        let mut reg = WorkerRegistry::default();
        reg.register(Bytes::from_static(b"worker-1"));
        reg.register(Bytes::from_static(b"worker-2"));

        let stats = BrokerStats {
            published: 120,
            commands: 4,
            registered: 3,
            unregistered: 1,
            dropped: 2,
        };

        let s_with_mon =
            format_broker_status(&args, Duration::from_secs(310), &stats, &reg, Some(5), Some(3));
        assert_eq!(
            s_with_mon,
            "[STATUS] broker | uptime 00:05:10\n  ROUTER : bind tcp://127.0.0.1:55550\n  PUB    : bind tcp://127.0.0.1:55555\n  monitor: on    debug: off\n  published               : 120\n  commands broadcast      : 4\n  registered / unregistered / dropped : 3 / 1 / 2\n  workers (2)             : worker-1, worker-2\n  ROUTER connections      : 5\n  PUB connections         : 3"
        );

        let args_no_mon = crate::ZmqArgs::try_parse_from([
            "zmqc",
            "--mode",
            "broker",
            "--endpoint",
            "tcp://127.0.0.1:55550",
            "--relay-endpoint",
            "tcp://127.0.0.1:55555",
        ])
        .unwrap();
        let empty_reg = WorkerRegistry::default();
        let empty_stats = BrokerStats::default();
        let s_no_mon = format_broker_status(
            &args_no_mon,
            Duration::from_secs(0),
            &empty_stats,
            &empty_reg,
            None,
            None,
        );
        assert_eq!(
            s_no_mon,
            "[STATUS] broker | uptime 00:00:00\n  ROUTER : bind tcp://127.0.0.1:55550\n  PUB    : bind tcp://127.0.0.1:55555\n  monitor: off   debug: off\n  published               : 0\n  commands broadcast      : 0\n  registered / unregistered / dropped : 0 / 0 / 0\n  workers (0)             : -\n  ROUTER connections      : n/a (use --monitor)\n  PUB connections         : n/a (use --monitor)"
        );
    }

    // AC-BRK-09: format_broker_status reflects settings variations (endpoints and debug)
    #[test]
    fn ac_brk_09_format_broker_settings() {
        use clap::Parser;
        let args = crate::ZmqArgs::try_parse_from([
            "zmqc",
            "--mode",
            "broker",
            "--endpoint",
            "tcp://127.0.0.1:55550",
            "--relay-endpoint",
            "tcp://127.0.0.1:55555",
            "--debug",
        ])
        .unwrap();
        let empty_reg = WorkerRegistry::default();
        let empty_stats = BrokerStats::default();
        let s = format_broker_status(&args, Duration::from_secs(0), &empty_stats, &empty_reg, None, None);
        assert!(s.contains("ROUTER : bind tcp://127.0.0.1:55550"));
        assert!(s.contains("PUB    : bind tcp://127.0.0.1:55555"));
        assert!(s.contains("monitor: off   debug: on"));

        let args_met = crate::ZmqArgs::try_parse_from([
            "zmqc",
            "--mode",
            "broker",
            "--endpoint",
            "tcp://127.0.0.1:55550",
            "--relay-endpoint",
            "tcp://127.0.0.1:55555",
            "--monitor",
            "--metrics-addr",
            "127.0.0.1:9100",
        ])
        .unwrap();
        let s_met = format_broker_status(
            &args_met,
            Duration::from_secs(0),
            &empty_stats,
            &empty_reg,
            Some(0),
            Some(0),
        );
        assert!(s_met.contains("metrics: http://127.0.0.1:9100/metrics"));
    }
}
