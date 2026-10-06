use clap::{Parser, ValueEnum};

mod modes;
use modes::{dealer_mode, pub_mode, router_mode, sub_mode};

#[derive(Parser, Debug, Clone)]
#[command(version, about, long_about = None)]
pub struct ZmqArgs {
    /// Operation mode: 'Pub', 'Sub', 'Dealer', or 'Router'
    #[arg(long, value_enum, ignore_case = true)]
    pub mode: Mode,

    /// If set, bind to endpoint; otherwise, connect (default: false, Router always binds).
    #[arg(long, default_value_t = false)]
    pub bind: bool,

    /// ZeroMQ endpoint to use (e.g., tcp://127.0.0.1:5555)
    #[arg(long)]
    pub endpoint: String,

    /// Optional topic for PUB/SUB filtering or DEALER/ROUTER frame prefix (default: "*")
    #[arg(long, default_value = "*")]
    pub topic: String,

    /// Input file to publish (PUB) or output file to save messages (SUB)
    #[arg(long)]
    pub file: Option<String>,

    /// Batch size for binary file streaming throttling (default: 1000)
    #[arg(long, default_value_t = 1000)]
    pub batch_size: usize,

    /// Throttle interval in milliseconds for binary file streaming (default: 100, 0 to disable)
    #[arg(long, default_value_t = 100)]
    pub throttle_interval_ms: u64,

    /// Socket identity for DEALER / ROUTER modes
    #[arg(long)]
    pub identity: Option<String>,

    /// Enable automatic ACK replies for ROUTER mode
    #[arg(long, default_value_t = false)]
    pub ack: bool,
}

/// Mode enum for publish/subscribe/dealer/router (used with clap's ValueEnum)
#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum Mode {
    Pub,
    Sub,
    Dealer,
    Router,
}

/// Validates argument combinations across modes.
pub fn validate_args(args: &ZmqArgs) -> Result<(), &'static str> {
    if args.ack && args.mode != Mode::Router {
        return Err("--ack is only valid in router mode");
    }
    if args.identity.is_some() && args.mode != Mode::Dealer && args.mode != Mode::Router {
        return Err("--identity is only valid in dealer or router mode");
    }
    if args.file.is_some() && (args.mode == Mode::Dealer || args.mode == Mode::Router) {
        return Err("--file is not supported in dealer or router mode");
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
        assert!(!a.bind);
        assert_eq!(a.topic, "*");
        assert_eq!(a.file, None);
        assert_eq!(a.batch_size, 1000);
        assert_eq!(a.throttle_interval_ms, 100);
        assert_eq!(a.identity, None);
        assert!(!a.ack);
    }

    // AC-CLI-02: missing required arguments are rejected.
    #[test]
    fn ac_cli_02_missing_required_args_are_rejected() {
        assert!(parse(&["--endpoint", "tcp://127.0.0.1:5555"]).is_err());
        assert!(parse(&["--mode", "pub"]).is_err());
        assert!(parse(&[]).is_err());
    }

    // AC-CLI-03: --mode is case-insensitive and limited to pub|sub|dealer|router.
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
        assert!(parse(&["--mode", "req", "--endpoint", "e"]).is_err());
    }

    // AC-CLI-04: --bind is an opt-in flag.
    #[test]
    fn ac_cli_04_bind_flag() {
        let a = parse(&["--mode", "pub", "--endpoint", "e", "--bind"]).unwrap();
        assert!(a.bind);
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
}

