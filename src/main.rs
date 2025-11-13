use clap::{Parser, ValueEnum};

mod modes;
use modes::{pub_mode, sub_mode};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct ZmqArgs {
    /// Operation mode: 'Pub' to publish, 'Sub' to subscribe
    #[arg(long, value_enum, ignore_case = true)]
    mode: Mode,

    /// If set, bind to endpoint; otherwise, connect.
    #[arg(long)]
    bind: bool,

    /// ZeroMQ endpoint to use (e.g., tcp://127.0.0.1:5555)
    #[arg(long)]
    endpoint: String,

    /// Optional topic for PUB/SUB filtering
    #[arg(long)]
    topic: Option<String>,

    #[arg(long)]
    file: Option<String>,

    #[arg(long)]
    buf: Option<i32>,

    #[arg(long)]
    hwm: Option<i32>,
}

/// Mode enum for publish/subscribe (used with clap's ValueEnum)
#[derive(Copy, Clone, Debug, ValueEnum)]
enum Mode {
    Pub,
    Sub,
}

fn main() {
    let args = ZmqArgs::parse();

    match args.mode {
        Mode::Pub => {
            pub_mode::run_publisher(args);
        }
        Mode::Sub => {
            sub_mode::run_subscriber(args);
        }
    }
}
