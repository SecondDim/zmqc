use std::error::Error;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use zeromq::{Socket, SocketRecv, SocketSend, XPubSocket, XSubSocket, ZmqMessage};

use super::console::{
    format_conn_count, format_console_help, format_unknown_command, format_uptime,
    parse_console_command, ConnCounter, ConsoleCommand, STDIN_EOF_NOTICE,
};
use super::sub_mode::{bytes_to_display, parse_message_frames};

/// Parses an XPUB subscription message frame into (is_subscribe, topic_display).
/// In ZeroMQ XPUB protocol, subscription frames start with byte 0x01 (subscribe)
/// or 0x00 (unsubscribe), followed by the topic bytes.
pub(crate) fn parse_sub_event(msg: &ZmqMessage) -> Option<(bool, String)> {
    if let Some(frame) = msg.get(0) {
        if frame.is_empty() {
            return None;
        }
        let prefix = frame[0];
        let topic_bytes = frame.slice(1..);
        let topic_disp = bytes_to_display(&topic_bytes);
        match prefix {
            1 => Some((true, topic_disp)),
            0 => Some((false, topic_disp)),
            _ => None,
        }
    } else {
        None
    }
}

/// Formats a forwarded data message for `--debug` output:
/// `[RELAY] [{seq}] {topic} => {payload}`.
pub(crate) fn format_proxy_debug_data(seq: usize, msg: &ZmqMessage) -> String {
    let (topic, payload) = parse_message_frames(msg);
    format!("[RELAY] [{}] {} => {}", seq, topic, payload)
}

/// Formats a subscription event for `--debug` output:
/// `[SUB] subscribe '{topic}'` or `[SUB] unsubscribe '{topic}'`.
pub(crate) fn format_proxy_debug_sub(is_subscribe: bool, topic: &str) -> String {
    let action = if is_subscribe {
        "subscribe"
    } else {
        "unsubscribe"
    };
    let topic_label = if topic.is_empty() { "*" } else { topic };
    format!("[SUB] {} '{}'", action, topic_label)
}

/// Formats the summary line printed when proxy exits:
/// `Proxy summary: forwarded {data_count} data message(s), {sub_count} subscription event(s).`
pub(crate) fn format_proxy_summary(data_count: usize, sub_count: usize) -> String {
    format!(
        "Proxy summary: forwarded {} data message(s), {} subscription event(s).",
        data_count, sub_count
    )
}

/// Formats the `/s` interactive status output (including startup settings) for Proxy mode.
pub(crate) fn format_proxy_status(
    args: &crate::ZmqArgs,
    uptime: Duration,
    data_count: usize,
    sub_count: usize,
    xpub_conns: Option<usize>,
    xsub_conns: Option<usize>,
) -> String {
    let xpub_role = if crate::resolve_bind_flag(args.mode, args.bind, args.connect) {
        "bind   "
    } else {
        "connect"
    };
    let xsub_role = if args.relay_bind { "bind   " } else { "connect" };
    let relay_ep = args.relay_endpoint.as_deref().unwrap_or("");
    let mon = if args.monitor { "on " } else { "off" };
    let dbg = if args.debug { "on" } else { "off" };
    format!(
        "[STATUS] proxy | uptime {}\n  XPUB : {} {}\n  XSUB : {} {}\n  monitor: {}   debug: {}\n  data messages forwarded : {}\n  subscription events     : {}\n  XPUB connections        : {}\n  XSUB connections        : {}",
        format_uptime(uptime),
        xpub_role,
        args.endpoint,
        xsub_role,
        relay_ep,
        mon,
        dbg,
        data_count,
        sub_count,
        format_conn_count(xpub_conns),
        format_conn_count(xsub_conns)
    )
}

/// Checks and warns about parameters that are ignored in proxy mode.
pub(crate) fn check_proxy_warnings(args: &crate::ZmqArgs) {
    if args.topic != "*" {
        eprintln!(
            "Warning: --topic is ignored in proxy mode (all topics are dynamically forwarded)."
        );
    }
    if args.file.is_some() {
        eprintln!("Warning: --file is ignored in proxy mode.");
    }
}

/// Runs the proxy loop using XPUB and XSUB sockets.
/// - XPUB (backend) faces subscribers and forwards their subscriptions to XSUB.
/// - XSUB (frontend) faces publishers and forwards their data messages to XPUB.
pub async fn run_proxy(args: crate::ZmqArgs) -> Result<(), Box<dyn Error>> {
    check_proxy_warnings(&args);

    let mut xpub = XPubSocket::new();
    let mut xsub = XSubSocket::new();

    let (xpub_counter, _xpub_monitor_handle) = if args.monitor {
        let c: ConnCounter = Arc::new(AtomicUsize::new(0));
        let h = super::monitor::spawn_socket_monitor_tracked(
            xpub.monitor(),
            Some("[XPUB]"),
            args.debug,
            Some(c.clone()),
        );
        (Some(c), Some(h))
    } else {
        (None, None)
    };

    let (xsub_counter, _xsub_monitor_handle) = if args.monitor {
        let c: ConnCounter = Arc::new(AtomicUsize::new(0));
        let h = super::monitor::spawn_socket_monitor_tracked(
            xsub.monitor(),
            Some("[XSUB]"),
            args.debug,
            Some(c.clone()),
        );
        (Some(c), Some(h))
    } else {
        (None, None)
    };

    let xpub_bind = crate::resolve_bind_flag(args.mode, args.bind, args.connect);
    if xpub_bind {
        xpub.bind(&args.endpoint).await?;
        println!("XPUB bound to {}", args.endpoint);
    } else {
        xpub.connect(&args.endpoint).await?;
        println!("XPUB connected to {}", args.endpoint);
    }

    let relay_endpoint = args
        .relay_endpoint
        .as_deref()
        .ok_or("--relay-endpoint is required in proxy mode")?;

    if args.relay_bind {
        xsub.bind(relay_endpoint).await?;
        println!("XSUB bound to {}", relay_endpoint);
    } else {
        xsub.connect(relay_endpoint).await?;
        println!("XSUB connected to {}", relay_endpoint);
    }

    println!("Proxy running (/h for commands, Ctrl+C to exit)...");

    let start_time = Instant::now();
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdin_active = true;
    let mut data_count: usize = 0;
    let mut sub_count: usize = 0;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nShutdown requested, exiting proxy.");
                println!("{}", format_proxy_summary(data_count, sub_count));
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
                            let xpub_c = xpub_counter.as_ref().map(|c| c.load(Ordering::Relaxed));
                            let xsub_c = xsub_counter.as_ref().map(|c| c.load(Ordering::Relaxed));
                            println!(
                                "{}",
                                format_proxy_status(
                                    &args,
                                    start_time.elapsed(),
                                    data_count,
                                    sub_count,
                                    xpub_c,
                                    xsub_c,
                                )
                            );
                        }
                        ConsoleCommand::Help => {
                            println!("{}", format_console_help());
                        }
                        ConsoleCommand::Quit => {
                            println!("Shutdown requested, exiting proxy.");
                            println!("{}", format_proxy_summary(data_count, sub_count));
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
            data_res = xsub.recv() => {
                let msg = data_res?;
                data_count += 1;
                if args.debug {
                    println!("{}", format_proxy_debug_data(data_count, &msg));
                }
                xpub.send(msg).await?;
            }
            sub_res = xpub.recv() => {
                let msg = sub_res?;
                sub_count += 1;
                if args.debug {
                    if let Some((is_sub, topic)) = parse_sub_event(&msg) {
                        println!("{}", format_proxy_debug_sub(is_sub, &topic));
                    }
                }
                xsub.send(msg).await?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    // AC-PRX-01: XPUB subscription event parsing for subscribe (0x01)
    #[test]
    fn ac_proxy_01_parse_sub_event_subscribe() {
        let msg = ZmqMessage::from(Bytes::from_static(b"\x01telemetry"));
        assert_eq!(
            parse_sub_event(&msg),
            Some((true, "telemetry".to_string()))
        );

        // Wildcard / empty topic
        let wildcard_msg = ZmqMessage::from(Bytes::from_static(b"\x01"));
        assert_eq!(parse_sub_event(&wildcard_msg), Some((true, "".to_string())));
    }

    // AC-PRX-02: XPUB subscription event parsing for unsubscribe (0x00)
    #[test]
    fn ac_proxy_02_parse_sub_event_unsubscribe() {
        let msg = ZmqMessage::from(Bytes::from_static(b"\x00sensor"));
        assert_eq!(
            parse_sub_event(&msg),
            Some((false, "sensor".to_string()))
        );
    }

    // AC-PRX-03: Invalid or non-prefixed message returns None
    #[test]
    fn ac_proxy_03_parse_sub_event_invalid() {
        let zero_byte_msg = ZmqMessage::from(Bytes::new());
        assert_eq!(parse_sub_event(&zero_byte_msg), None);

        let other_prefix = ZmqMessage::from(Bytes::from_static(b"\x02unknown"));
        assert_eq!(parse_sub_event(&other_prefix), None);
    }

    // AC-PRX-04: Debug data formatting [RELAY] [{seq}] {topic} => {payload}
    #[test]
    fn ac_proxy_04_format_proxy_debug_data() {
        let mut msg = ZmqMessage::from(Bytes::from_static(b"sensor"));
        msg.push_back(Bytes::from_static(b"25.4"));
        assert_eq!(
            format_proxy_debug_data(1, &msg),
            "[RELAY] [1] sensor => 25.4"
        );
    }

    // AC-PRX-05: Debug subscription formatting [SUB] subscribe/unsubscribe '{topic}'
    #[test]
    fn ac_proxy_05_format_proxy_debug_sub() {
        assert_eq!(
            format_proxy_debug_sub(true, "sensor"),
            "[SUB] subscribe 'sensor'"
        );
        assert_eq!(
            format_proxy_debug_sub(false, "sensor"),
            "[SUB] unsubscribe 'sensor'"
        );
        assert_eq!(
            format_proxy_debug_sub(true, ""),
            "[SUB] subscribe '*'"
        );
    }

    // AC-PRX-06: Exit summary formatting
    #[test]
    fn ac_proxy_06_format_proxy_summary() {
        assert_eq!(
            format_proxy_summary(42, 3),
            "Proxy summary: forwarded 42 data message(s), 3 subscription event(s)."
        );
    }

    // AC-PRX-07: format_proxy_status with settings and connection counts
    #[test]
    fn ac_proxy_07_format_proxy_status() {
        use clap::Parser;
        let args = crate::ZmqArgs::try_parse_from([
            "zmqc",
            "--mode",
            "proxy",
            "--endpoint",
            "tcp://127.0.0.1:5556",
            "--relay-endpoint",
            "tcp://127.0.0.1:5555",
            "--monitor",
            "--debug",
        ])
        .unwrap();

        let s_with_mon =
            format_proxy_status(&args, Duration::from_secs(62), 42, 3, Some(2), Some(1));
        assert_eq!(
            s_with_mon,
            "[STATUS] proxy | uptime 00:01:02\n  XPUB : bind    tcp://127.0.0.1:5556\n  XSUB : connect tcp://127.0.0.1:5555\n  monitor: on    debug: on\n  data messages forwarded : 42\n  subscription events     : 3\n  XPUB connections        : 2\n  XSUB connections        : 1"
        );

        let args_no_mon = crate::ZmqArgs::try_parse_from([
            "zmqc",
            "--mode",
            "proxy",
            "--endpoint",
            "tcp://127.0.0.1:5556",
            "--relay-endpoint",
            "tcp://127.0.0.1:5555",
        ])
        .unwrap();
        let s_no_mon =
            format_proxy_status(&args_no_mon, Duration::from_secs(0), 0, 0, None, None);
        assert_eq!(
            s_no_mon,
            "[STATUS] proxy | uptime 00:00:00\n  XPUB : bind    tcp://127.0.0.1:5556\n  XSUB : connect tcp://127.0.0.1:5555\n  monitor: off   debug: off\n  data messages forwarded : 0\n  subscription events     : 0\n  XPUB connections        : n/a (use --monitor)\n  XSUB connections        : n/a (use --monitor)"
        );
    }

    // AC-PRX-08: format_proxy_status reflects bind/connect and relay-bind variations
    #[test]
    fn ac_proxy_08_format_proxy_settings_variations() {
        use clap::Parser;
        let args1 = crate::ZmqArgs::try_parse_from([
            "zmqc",
            "--mode",
            "proxy",
            "--endpoint",
            "tcp://127.0.0.1:5556",
            "--relay-endpoint",
            "tcp://127.0.0.1:5555",
            "--debug",
        ])
        .unwrap();
        let s1 = format_proxy_status(&args1, Duration::from_secs(10), 0, 0, None, None);
        assert!(s1.contains("XPUB : bind    tcp://127.0.0.1:5556"));
        assert!(s1.contains("XSUB : connect tcp://127.0.0.1:5555"));
        assert!(s1.contains("monitor: off   debug: on"));

        let args2 = crate::ZmqArgs::try_parse_from([
            "zmqc",
            "--mode",
            "proxy",
            "--endpoint",
            "tcp://127.0.0.1:5556",
            "--connect",
            "--relay-endpoint",
            "tcp://127.0.0.1:5555",
            "--relay-bind",
            "--monitor",
        ])
        .unwrap();
        let s2 = format_proxy_status(&args2, Duration::from_secs(20), 5, 1, Some(0), Some(0));
        assert!(s2.contains("XPUB : connect tcp://127.0.0.1:5556"));
        assert!(s2.contains("XSUB : bind    tcp://127.0.0.1:5555"));
        assert!(s2.contains("monitor: on    debug: off"));
    }
}
