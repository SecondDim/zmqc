use clap::{Parser, ValueEnum};

mod modes;
use modes::{dealer_mode, proxy_mode, pub_mode, router_mode, sub_mode};

#[derive(Parser, Debug, Clone)]
#[command(version, about, long_about = None)]
pub struct ZmqArgs {
    /// Operation mode: 'Pub', 'Sub', 'Dealer', 'Router', or 'Proxy'
    #[arg(short, long, value_enum, ignore_case = true)]
    pub mode: Mode,

    /// If set, bind to endpoint; otherwise, connect (supports --bind=false; Proxy XPUB defaults to bind; Router always binds).
    #[arg(short, long, default_missing_value = "true", num_args = 0..=1)]
    pub bind: Option<bool>,

    /// Explicitly connect to endpoint (forces bind to false). Applies to all modes.
    #[arg(short, long, default_value_t = false)]
    pub connect: bool,

    /// ZeroMQ endpoint to use (e.g., tcp://127.0.0.1:5555)
    #[arg(short, long)]
    pub endpoint: String,

    /// Secondary ZeroMQ endpoint used for XSUB in Proxy mode (connecting to publishers)
    #[arg(long)]
    pub relay_endpoint: Option<String>,

    /// If set, bind XSUB to relay-endpoint (default in Proxy mode: connect)
    #[arg(long, default_value_t = false)]
    pub relay_bind: bool,

    /// Optional topic for PUB/SUB filtering or DEALER/ROUTER frame prefix (default: "*")
    #[arg(short, long, default_value = "*")]
    pub topic: String,

    /// Input file to publish (PUB) or output file to save messages (SUB)
    #[arg(short, long)]
    pub file: Option<String>,

    /// Batch size for binary file streaming throttling (default: 1000)
    #[arg(long, default_value_t = 1000)]
    pub batch_size: usize,

    /// Throttle interval in milliseconds for binary file streaming (default: 100, 0 to disable)
    #[arg(long, default_value_t = 100)]
    pub throttle_interval_ms: u64,

    /// Socket identity for DEALER / ROUTER modes
    #[arg(short, long)]
    pub identity: Option<String>,

    /// Enable automatic ACK replies for ROUTER mode
    #[arg(short, long, default_value_t = false)]
    pub ack: bool,

    /// Monitor and display peer connection and disconnection events
    #[arg(long, default_value_t = false)]
    pub monitor: bool,

    /// Enable verbose debug output (shows all internal socket events when used with --monitor)
    #[arg(short, long, default_value_t = false)]
    pub debug: bool,
}

impl ZmqArgs {
    /// Resolves whether the primary socket should bind.
    pub fn is_bind(&self) -> bool {
        resolve_bind_flag(self.mode, self.bind, self.connect)
    }
}

/// Resolves whether the primary socket should bind based on mode, --bind, and --connect.
pub fn resolve_bind_flag(mode: Mode, bind_arg: Option<bool>, connect_arg: bool) -> bool {
    if connect_arg {
        return false;
    }
    if let Some(b) = bind_arg {
        return b;
    }
    match mode {
        Mode::Router | Mode::Proxy => true,
        _ => false,
    }
}

/// Mode enum for publish/subscribe/dealer/router/proxy (used with clap's ValueEnum)
#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum Mode {
    Pub,
    Sub,
    Dealer,
    Router,
    Proxy,
}

/// Validates argument combinations across modes.
pub fn validate_args(args: &ZmqArgs) -> Result<(), &'static str> {
    if args.bind == Some(true) && args.connect {
        return Err("--bind and --connect cannot both be specified");
    }
    if args.ack && args.mode != Mode::Router {
        return Err("--ack is only valid in router mode");
    }
    if args.identity.is_some() && args.mode != Mode::Dealer && args.mode != Mode::Router {
        return Err("--identity is only valid in dealer or router mode");
    }
    if args.file.is_some() && (args.mode == Mode::Dealer || args.mode == Mode::Router) {
        return Err("--file is not supported in dealer or router mode");
    }
    if args.mode == Mode::Proxy && args.relay_endpoint.is_none() {
        return Err("--relay-endpoint is required in proxy mode");
    }
    if args.mode != Mode::Proxy && args.relay_endpoint.is_some() {
        return Err("--relay-endpoint is only valid in proxy mode");
    }
    if args.mode != Mode::Proxy && args.relay_bind {
        return Err("--relay-bind is only valid in proxy mode");
    }
    Ok(())
}

/// Formats an error into a user-friendly message with context.
pub fn format_error(e: &(dyn std::error::Error + 'static), mode: Mode, endpoint: &str) -> String {
    let msg = e.to_string();
    if msg.contains("Provided sockets combination is not compatible") {
        let (local_sock, allowed_peer) = match mode {
            Mode::Pub => ("PUB", "SUB or XSUB"),
            Mode::Sub => ("SUB", "PUB or XPUB"),
            Mode::Dealer => ("DEALER", "ROUTER, DEALER, or REP"),
            Mode::Router => ("ROUTER", "DEALER, ROUTER, or REQ"),
            Mode::Proxy => ("XPUB/XSUB", "SUB/XPUB (for XPUB) or PUB/XSUB (for XSUB)"),
        };
        format!(
            "Error: Provided sockets combination is not compatible\n\n\
            [Socket Incompatibility Explanation]\n\
            - Local socket: {} (mode: {:?})\n\
            - Target endpoint: {}\n\
            - A {} socket can ONLY connect to a {} socket.\n\n\
            [Troubleshooting]\n\
            1. Ensure both sides are running compatible socket modes (e.g. PUB<->SUB or DEALER<->ROUTER).\n\
            2. Verify that the port '{}' is not already in use by an incompatible ZeroMQ socket or service.",
            local_sock, mode, endpoint, local_sock, allowed_peer, endpoint
        )
    } else {
        format!("Error: {}", msg)
    }
}

#[tokio::main]
async fn main() {
    let args = ZmqArgs::parse();

    if let Err(e) = validate_args(&args) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }

    let res = match args.mode {
        Mode::Pub => pub_mode::run_publisher(args.clone()).await,
        Mode::Sub => sub_mode::run_subscriber(args.clone()).await,
        Mode::Dealer => dealer_mode::run_dealer(args.clone()).await,
        Mode::Router => router_mode::run_router(args.clone()).await,
        Mode::Proxy => proxy_mode::run_proxy(args.clone()).await,
    };

    if let Err(e) = res {
        eprintln!("{}", format_error(e.as_ref(), args.mode, &args.endpoint));
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<ZmqArgs, clap::Error> {
        let mut full = vec!["zmqc"];
        full.extend_from_slice(args);
        ZmqArgs::try_parse_from(full)
    }

    // AC-CLI-01: only --mode and --endpoint are required; everything else has a safe default.
    #[test]
    fn ac_cli_01_minimal_args_use_defaults() {
        let a = parse(&["--mode", "sub", "--endpoint", "tcp://127.0.0.1:5555"]).unwrap();
        assert!(matches!(a.mode, Mode::Sub));
        assert_eq!(a.endpoint, "tcp://127.0.0.1:5555");
        assert!(!a.is_bind());
        assert_eq!(a.bind, None);
        assert!(!a.connect);
        assert_eq!(a.relay_endpoint, None);
        assert!(!a.relay_bind);
        assert_eq!(a.topic, "*");
        assert_eq!(a.file, None);
        assert_eq!(a.batch_size, 1000);
        assert_eq!(a.throttle_interval_ms, 100);
        assert_eq!(a.identity, None);
        assert!(!a.ack);
        assert!(!a.monitor);
        assert!(!a.debug);
    }

    // AC-CLI-02: missing required arguments are rejected.
    #[test]
    fn ac_cli_02_missing_required_args_are_rejected() {
        assert!(parse(&["--endpoint", "tcp://127.0.0.1:5555"]).is_err());
        assert!(parse(&["--mode", "pub"]).is_err());
        assert!(parse(&[]).is_err());
    }

    // AC-CLI-03: --mode is case-insensitive and limited to pub|sub|dealer|router|proxy.
    #[test]
    fn ac_cli_03_mode_is_case_insensitive_and_validated() {
        for m in ["pub", "PUB", "Pub"] {
            let a = parse(&["--mode", m, "--endpoint", "e"]).unwrap();
            assert!(matches!(a.mode, Mode::Pub), "{m}");
        }
        for m in ["sub", "SUB", "Sub"] {
            let a = parse(&["--mode", m, "--endpoint", "e"]).unwrap();
            assert!(matches!(a.mode, Mode::Sub), "{m}");
        }
        for m in ["dealer", "DEALER", "Dealer"] {
            let a = parse(&["--mode", m, "--endpoint", "e"]).unwrap();
            assert!(matches!(a.mode, Mode::Dealer), "{m}");
        }
        for m in ["router", "ROUTER", "Router"] {
            let a = parse(&["--mode", m, "--endpoint", "e"]).unwrap();
            assert!(matches!(a.mode, Mode::Router), "{m}");
        }
        for m in ["proxy", "PROXY", "Proxy"] {
            let a = parse(&[
                "--mode",
                m,
                "--endpoint",
                "e",
                "--relay-endpoint",
                "pe",
            ])
            .unwrap();
            assert!(matches!(a.mode, Mode::Proxy), "{m}");
        }
        assert!(parse(&["--mode", "req", "--endpoint", "e"]).is_err());
    }

    // AC-CLI-04: --bind is an opt-in flag.
    #[test]
    fn ac_cli_04_bind_flag() {
        let a = parse(&["--mode", "pub", "--endpoint", "e", "--bind"]).unwrap();
        assert!(a.is_bind());
        assert_eq!(a.bind, Some(true));

        let a_false = parse(&["--mode", "pub", "--endpoint", "e", "--bind=false"]).unwrap();
        assert!(!a_false.is_bind());
        assert_eq!(a_false.bind, Some(false));
    }

    // AC-CLI-05: --topic and --file are carried through unchanged.
    #[test]
    fn ac_cli_05_topic_and_file_are_parsed() {
        let a = parse(&[
            "--mode", "pub", "--endpoint", "e", "--topic", "sensor", "--file", "data.txt",
        ])
        .unwrap();
        assert_eq!(a.topic, "sensor");
        assert_eq!(a.file.as_deref(), Some("data.txt"));
    }

    #[test]
    fn ac_cli_format_error_handles_socket_incompatibility() {
        let err = std::io::Error::new(
            std::io::ErrorKind::Other,
            "Provided sockets combination is not compatible",
        );
        let msg = format_error(&err, Mode::Pub, "tcp://127.0.0.1:5555");
        assert!(msg.contains("Socket Incompatibility Explanation"));
        assert!(msg.contains("PUB"));
        assert!(msg.contains("SUB or XSUB"));
        assert!(msg.contains("tcp://127.0.0.1:5555"));

        let msg_dlr = format_error(&err, Mode::Dealer, "tcp://127.0.0.1:5555");
        assert!(msg_dlr.contains("DEALER"));
        assert!(msg_dlr.contains("ROUTER, DEALER, or REP"));

        let msg_prx = format_error(&err, Mode::Proxy, "tcp://127.0.0.1:5555");
        assert!(msg_prx.contains("XPUB/XSUB"));
    }

    // AC-CLI-06: throttling options accept numbers (0 allowed) and reject garbage/negatives.
    #[test]
    fn ac_cli_06_throttle_options() {
        let a = parse(&[
            "--mode", "pub", "--endpoint", "e", "--batch-size", "500",
            "--throttle-interval-ms", "0",
        ])
        .unwrap();
        assert_eq!(a.batch_size, 500);
        assert_eq!(a.throttle_interval_ms, 0);

        assert!(parse(&["--mode", "pub", "--endpoint", "e", "--batch-size", "abc"]).is_err());
        assert!(parse(&["--mode", "pub", "--endpoint", "e", "--batch-size", "-1"]).is_err());
        assert!(
            parse(&["--mode", "pub", "--endpoint", "e", "--throttle-interval-ms", "-5"]).is_err()
        );
    }

    // AC-CLI-07: the legacy libzmq-only options no longer exist.
    #[test]
    fn ac_cli_07_legacy_buf_and_hwm_are_rejected() {
        assert!(parse(&["--mode", "pub", "--endpoint", "e", "--buf", "1024"]).is_err());
        assert!(parse(&["--mode", "pub", "--endpoint", "e", "--hwm", "1000"]).is_err());
    }

    // AC-CLI-08 & 09: identity and ack flags parsing and defaults
    #[test]
    fn ac_cli_08_and_09_identity_and_ack() {
        let a = parse(&["--mode", "router", "--endpoint", "e", "--identity", "srv1", "--ack"]).unwrap();
        assert_eq!(a.identity.as_deref(), Some("srv1"));
        assert!(a.ack);

        let d = parse(&["--mode", "dealer", "--endpoint", "e"]).unwrap();
        assert_eq!(d.identity, None);
        assert!(!d.ack);
    }

    // AC-CLI-10: validate_args combinations
    #[test]
    fn ac_cli_10_validate_args_combinations() {
        // Valid dealer/router
        let ok_dealer = parse(&["--mode", "dealer", "--endpoint", "e", "--identity", "c1"]).unwrap();
        assert!(validate_args(&ok_dealer).is_ok());

        let ok_router = parse(&["--mode", "router", "--endpoint", "e", "--identity", "r1", "--ack"]).unwrap();
        assert!(validate_args(&ok_router).is_ok());

        // --ack on non-router is rejected
        let bad_ack_dealer = parse(&["--mode", "dealer", "--endpoint", "e", "--ack"]).unwrap();
        assert_eq!(validate_args(&bad_ack_dealer), Err("--ack is only valid in router mode"));

        let bad_ack_sub = parse(&["--mode", "sub", "--endpoint", "e", "--ack"]).unwrap();
        assert_eq!(validate_args(&bad_ack_sub), Err("--ack is only valid in router mode"));

        // --identity on pub/sub is rejected
        let bad_id_pub = parse(&["--mode", "pub", "--endpoint", "e", "--identity", "p"]).unwrap();
        assert_eq!(validate_args(&bad_id_pub), Err("--identity is only valid in dealer or router mode"));

        let bad_id_sub = parse(&["--mode", "sub", "--endpoint", "e", "--identity", "s"]).unwrap();
        assert_eq!(validate_args(&bad_id_sub), Err("--identity is only valid in dealer or router mode"));

        // --file on dealer/router is rejected
        let bad_file_dealer = parse(&["--mode", "dealer", "--endpoint", "e", "--file", "f.txt"]).unwrap();
        assert_eq!(validate_args(&bad_file_dealer), Err("--file is not supported in dealer or router mode"));

        let bad_file_router = parse(&["--mode", "router", "--endpoint", "e", "--file", "f.txt"]).unwrap();
        assert_eq!(validate_args(&bad_file_router), Err("--file is not supported in dealer or router mode"));
    }

    // AC-CLI-11: proxy mode arguments and required relay-endpoint
    #[test]
    fn ac_cli_11_proxy_mode_args_and_validation() {
        let ok_proxy = parse(&[
            "--mode",
            "proxy",
            "--endpoint",
            "tcp://127.0.0.1:5556",
            "--relay-endpoint",
            "tcp://127.0.0.1:5555",
        ])
        .unwrap();
        assert!(validate_args(&ok_proxy).is_ok());
        assert!(ok_proxy.is_bind()); // XPUB defaults to bind
        assert!(!ok_proxy.relay_bind); // XSUB defaults to connect

        // Missing --relay-endpoint in proxy mode
        let bad_proxy = parse(&["--mode", "proxy", "--endpoint", "tcp://127.0.0.1:5556"]).unwrap();
        assert_eq!(
            validate_args(&bad_proxy),
            Err("--relay-endpoint is required in proxy mode")
        );

        // --relay-endpoint in non-proxy mode
        let bad_sub = parse(&[
            "--mode",
            "sub",
            "--endpoint",
            "tcp://127.0.0.1:5556",
            "--relay-endpoint",
            "tcp://127.0.0.1:5555",
        ])
        .unwrap();
        assert_eq!(
            validate_args(&bad_sub),
            Err("--relay-endpoint is only valid in proxy mode")
        );
    }

    // AC-CLI-12: relay-bind, connect, bind=false, and debug flags
    #[test]
    fn ac_cli_12_proxy_bind_and_connect_flags() {
        // --connect sets bind to false in proxy mode
        let p_connect = parse(&[
            "--mode",
            "proxy",
            "--endpoint",
            "tcp://127.0.0.1:5556",
            "--relay-endpoint",
            "tcp://127.0.0.1:5555",
            "--connect",
        ])
        .unwrap();
        assert!(!p_connect.is_bind());

        // --bind=false sets bind to false
        let p_bind_false = parse(&[
            "--mode",
            "proxy",
            "--endpoint",
            "tcp://127.0.0.1:5556",
            "--relay-endpoint",
            "tcp://127.0.0.1:5555",
            "--bind=false",
        ])
        .unwrap();
        assert!(!p_bind_false.is_bind());

        // --relay-bind enables bind for XSUB
        let p_relay_bind = parse(&[
            "--mode",
            "proxy",
            "--endpoint",
            "tcp://127.0.0.1:5556",
            "--relay-endpoint",
            "tcp://127.0.0.1:5555",
            "--relay-bind",
            "--debug",
        ])
        .unwrap();
        assert!(p_relay_bind.relay_bind);
        assert!(p_relay_bind.debug);
        assert!(validate_args(&p_relay_bind).is_ok());

        // Conflicting --bind and --connect
        let p_conflict = parse(&[
            "--mode",
            "proxy",
            "--endpoint",
            "e",
            "--relay-endpoint",
            "pe",
            "--bind",
            "--connect",
        ])
        .unwrap();
        assert_eq!(
            validate_args(&p_conflict),
            Err("--bind and --connect cannot both be specified")
        );

        // --relay-bind on non-proxy mode is rejected
        let bad_sub_pbind = parse(&["--mode", "sub", "--endpoint", "e", "--relay-bind"]).unwrap();
        assert_eq!(
            validate_args(&bad_sub_pbind),
            Err("--relay-bind is only valid in proxy mode")
        );

        // --debug on non-proxy mode is now allowed globally
        let sub_debug = parse(&["--mode", "sub", "--endpoint", "e", "--debug"]).unwrap();
        assert!(validate_args(&sub_debug).is_ok());
    }

    // AC-CLI-13: In proxy mode, --topic and --file are allowed (only produce runtime warnings)
    #[test]
    fn ac_cli_13_proxy_allows_topic_and_file() {
        let p_warn = parse(&[
            "--mode",
            "proxy",
            "--endpoint",
            "e",
            "--relay-endpoint",
            "pe",
            "--topic",
            "mytopic",
            "--file",
            "test.txt",
        ])
        .unwrap();
        assert!(validate_args(&p_warn).is_ok());
    }

    // AC-CLI-14: In proxy mode, --ack and --identity are rejected
    #[test]
    fn ac_cli_14_proxy_rejects_ack_and_identity() {
        let p_ack = parse(&[
            "--mode",
            "proxy",
            "--endpoint",
            "e",
            "--relay-endpoint",
            "pe",
            "--ack",
        ])
        .unwrap();
        assert_eq!(
            validate_args(&p_ack),
            Err("--ack is only valid in router mode")
        );

        let p_id = parse(&[
            "--mode",
            "proxy",
            "--endpoint",
            "e",
            "--relay-endpoint",
            "pe",
            "--identity",
            "pxy1",
        ])
        .unwrap();
        assert_eq!(
            validate_args(&p_id),
            Err("--identity is only valid in dealer or router mode")
        );
    }

    // AC-CLI-15: --monitor flag and global --debug flag validation
    #[test]
    fn ac_cli_15_monitor_and_global_debug() {
        let m = parse(&["--mode", "pub", "--endpoint", "e", "--monitor", "--debug"]).unwrap();
        assert!(m.monitor);
        assert!(m.debug);
        assert!(validate_args(&m).is_ok());

        let m_router = parse(&["--mode", "router", "--endpoint", "e", "--monitor"]).unwrap();
        assert!(m_router.monitor);
        assert!(!m_router.debug);
        assert!(validate_args(&m_router).is_ok());

        let m_proxy = parse(&[
            "--mode",
            "proxy",
            "--endpoint",
            "e",
            "--relay-endpoint",
            "pe",
            "--monitor",
        ])
        .unwrap();
        assert!(m_proxy.monitor);
        assert!(validate_args(&m_proxy).is_ok());
    }

    // AC-CLI-16: Short options support (-m, -e, -b, -c, -t, -f, -i, -a, -d)
    #[test]
    fn ac_cli_16_short_options_support() {
        let a = parse(&[
            "-m", "sub",
            "-e", "tcp://127.0.0.1:5555",
            "-t", "alerts",
            "-f", "out.log",
            "-d",
        ])
        .unwrap();
        assert!(matches!(a.mode, Mode::Sub));
        assert_eq!(a.endpoint, "tcp://127.0.0.1:5555");
        assert_eq!(a.topic, "alerts");
        assert_eq!(a.file.as_deref(), Some("out.log"));
        assert!(a.debug);

        // Dealer mode with -i and -c
        let d = parse(&["-m", "dealer", "-e", "tcp://127.0.0.1:5555", "-i", "d1", "-c"]).unwrap();
        assert!(matches!(d.mode, Mode::Dealer));
        assert_eq!(d.identity.as_deref(), Some("d1"));
        assert!(d.connect);
        assert!(!d.is_bind());

        // Router mode with -b and -a
        let r = parse(&["-m", "router", "-e", "tcp://127.0.0.1:5555", "-b", "-a"]).unwrap();
        assert!(matches!(r.mode, Mode::Router));
        assert_eq!(r.bind, Some(true));
        assert!(r.ack);

        // -b false
        let r_nobind = parse(&["-m", "pub", "-e", "tcp://127.0.0.1:5555", "-b=false"]).unwrap();
        assert_eq!(r_nobind.bind, Some(false));

        // Combined bool flags (-cd)
        let comb = parse(&["-m", "pub", "-e", "tcp://127.0.0.1:5555", "-cd"]).unwrap();
        assert!(comb.connect);
        assert!(comb.debug);
    }
}
