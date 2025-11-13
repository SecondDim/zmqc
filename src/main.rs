use clap::{Parser, ValueEnum};

mod modes;
use modes::{pub_mode, sub_mode};


#[derive(Parser, Debug, Clone)]
#[command(version, about, long_about = None)]
pub struct ZmqArgs {
    /// Operation mode: 'Pub' to publish, 'Sub' to subscribe
    #[arg(long, value_enum, ignore_case = true)]
    pub mode: Mode,

    /// If set, bind to endpoint; otherwise, connect (default: false).
    #[arg(long, default_value_t = false)]
    pub bind: bool,

    /// ZeroMQ endpoint to use (e.g., tcp://127.0.0.1:5555)
    #[arg(long)]
    pub endpoint: String,

    /// Optional topic for PUB/SUB filtering (default: "*")
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
}

/// Mode enum for publish/subscribe (used with clap's ValueEnum)
#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum Mode {
    Pub,
    Sub,
}

/// Formats an error into a user-friendly message with context.
pub fn format_error(e: &(dyn std::error::Error + 'static), mode: Mode, endpoint: &str) -> String {
    let msg = e.to_string();
    if msg.contains("Provided sockets combination is not compatible") {
        let (local_sock, allowed_peer) = match mode {
            Mode::Pub => ("PUB", "SUB or XSUB"),
            Mode::Sub => ("SUB", "PUB or XPUB"),
        };
        format!(
            "Error: Provided sockets combination is not compatible\n\n\
            [Socket Incompatibility Explanation]\n\
            - Local socket: {} (mode: {:?})\n\
            - Target endpoint: {}\n\
            - A {} socket can ONLY connect to a {} socket.\n\
            - PUB-to-PUB and SUB-to-SUB connections are NOT allowed by ZeroMQ.\n\n\
            [Troubleshooting]\n\
            1. Ensure you didn't launch both instances in the same mode (e.g. both pub or both sub).\n\
            2. If you are running a publisher and subscriber, ensure one is 'pub' and the other is 'sub'.\n\
            3. Verify that the port '{}' is not already in use by an incompatible ZeroMQ socket or service.",
            local_sock, mode, endpoint, local_sock, allowed_peer, endpoint
        )
    } else {
        format!("Error: {}", msg)
    }
}

#[tokio::main]
async fn main() {
    let args = ZmqArgs::parse();

    let res = match args.mode {
        Mode::Pub => pub_mode::run_publisher(args.clone()).await,
        Mode::Sub => sub_mode::run_subscriber(args.clone()).await,
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
    }

    // AC-CLI-02: missing required arguments are rejected.
    #[test]
    fn ac_cli_02_missing_required_args_are_rejected() {
        assert!(parse(&["--endpoint", "tcp://127.0.0.1:5555"]).is_err());
        assert!(parse(&["--mode", "pub"]).is_err());
        assert!(parse(&[]).is_err());
    }

    // AC-CLI-03: --mode is case-insensitive and limited to pub|sub.
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
}

