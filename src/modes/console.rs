use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use zeromq::SocketEvent;

/// Shared atomic connection counter updated by the socket monitor task when `--monitor` is active.
pub(crate) type ConnCounter = Arc<AtomicUsize>;

/// Notice printed once when stdin reaches EOF in `proxy` or `broker` mode.
pub(crate) const STDIN_EOF_NOTICE: &str =
    "stdin reached EOF, continuing to run (Ctrl+C to exit)...";

/// Parsed interactive console command from stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConsoleCommand {
    /// `/s` (or direct Enter): Display runtime status and settings.
    Status,
    /// `/h` or `/?`: Display command help text.
    Help,
    /// `/q`: Graceful shutdown (equivalent to Ctrl+C).
    Quit,
    /// Unrecognized input.
    Unknown(String),
}

/// Parses a single stdin line into a `ConsoleCommand` (whitespace-trimmed, case-insensitive).
/// Directly pressing Enter (empty or whitespace-only line) defaults to `ConsoleCommand::Status`.
/// `/?` is equivalent to `/h`.
pub(crate) fn parse_console_command(line: &str) -> ConsoleCommand {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return ConsoleCommand::Status;
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "/s" => ConsoleCommand::Status,
        "/h" | "/?" => ConsoleCommand::Help,
        "/q" => ConsoleCommand::Quit,
        _ => ConsoleCommand::Unknown(trimmed.to_string()),
    }
}

/// Formats an elapsed `Duration` as `HH:MM:SS` (hours may exceed 24, e.g. `100:00:05`).
pub(crate) fn format_uptime(d: Duration) -> String {
    let total_secs = d.as_secs();
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    format!("{:02}:{:02}:{:02}", hours, mins, secs)
}

/// Formats a connection count option: `Some(n)` => `"{n}"`, `None` => `"n/a (use --monitor)"`.
pub(crate) fn format_conn_count(c: Option<usize>) -> String {
    match c {
        Some(n) => n.to_string(),
        None => "n/a (use --monitor)".to_string(),
    }
}

/// Formats the `/h` / `/?` help message.
pub(crate) fn format_console_help() -> String {
    "Commands: [Enter] or /s status | /h or /? help | /q quit (Ctrl+C also exits)".to_string()
}

/// Formats an unknown command error message.
pub(crate) fn format_unknown_command(cmd: &str) -> String {
    format!("Unknown command '{}'. Type /h for help.", cmd)
}

/// Applies a `SocketEvent` to an `AtomicUsize` connection counter:
/// - `Accepted` / `Connected` increments by 1.
/// - `Disconnected` saturating-decrements by 1 (never underflows below 0).
/// - All other events leave the counter unchanged.
pub(crate) fn apply_connection_event(ev: &SocketEvent, c: &AtomicUsize) {
    match ev {
        SocketEvent::Accepted(_, _) | SocketEvent::Connected(_, _) => {
            c.fetch_add(1, Ordering::Relaxed);
        }
        SocketEvent::Disconnected(_) => {
            let mut cur = c.load(Ordering::Relaxed);
            while cur > 0 {
                match c.compare_exchange_weak(
                    cur,
                    cur - 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(actual) => cur = actual,
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-CON-01: parse_console_command maps /s, /h (or /?), /q and direct Enter to Status; /ss is unknown
    #[test]
    fn ac_con_01_parse_console_command() {
        assert_eq!(parse_console_command("/s"), ConsoleCommand::Status);
        assert_eq!(parse_console_command("  /S \r\n"), ConsoleCommand::Status);
        // /ss was removed and is now treated as unknown command
        assert_eq!(
            parse_console_command("/ss"),
            ConsoleCommand::Unknown("/ss".to_string())
        );
        assert_eq!(
            parse_console_command(" /SS "),
            ConsoleCommand::Unknown("/SS".to_string())
        );

        // /h and /? are equivalent
        assert_eq!(parse_console_command("/h"), ConsoleCommand::Help);
        assert_eq!(parse_console_command("/H"), ConsoleCommand::Help);
        assert_eq!(parse_console_command("/?"), ConsoleCommand::Help);
        assert_eq!(parse_console_command(" /? \r\n"), ConsoleCommand::Help);

        assert_eq!(parse_console_command("/q"), ConsoleCommand::Quit);
        assert_eq!(parse_console_command("/Q"), ConsoleCommand::Quit);

        // Direct Enter (empty string or whitespace) defaults to Status
        assert_eq!(parse_console_command(""), ConsoleCommand::Status);
        assert_eq!(parse_console_command("   \t\r\n "), ConsoleCommand::Status);

        assert_eq!(
            parse_console_command("s"),
            ConsoleCommand::Unknown("s".to_string())
        );
        assert_eq!(
            parse_console_command("/status"),
            ConsoleCommand::Unknown("/status".to_string())
        );
        assert_eq!(
            parse_console_command("  /s extra  "),
            ConsoleCommand::Unknown("/s extra".to_string())
        );
        assert_eq!(
            parse_console_command("?"),
            ConsoleCommand::Unknown("?".to_string())
        );
    }

    // AC-CON-02: format_uptime formats Duration as HH:MM:SS including >24h
    #[test]
    fn ac_con_02_format_uptime() {
        assert_eq!(format_uptime(Duration::from_secs(0)), "00:00:00");
        assert_eq!(format_uptime(Duration::from_secs(62)), "00:01:02");
        assert_eq!(format_uptime(Duration::from_secs(3661)), "01:01:01");
        assert_eq!(
            format_uptime(Duration::from_secs(100 * 3600 + 5)),
            "100:00:05"
        );
    }

    // AC-CON-03: format_conn_count, format_console_help, and format_unknown_command
    #[test]
    fn ac_con_03_format_helpers() {
        assert_eq!(format_conn_count(Some(0)), "0");
        assert_eq!(format_conn_count(Some(5)), "5");
        assert_eq!(format_conn_count(None), "n/a (use --monitor)");

        assert_eq!(
            format_console_help(),
            "Commands: [Enter] or /s status | /h or /? help | /q quit (Ctrl+C also exits)"
        );
        assert_eq!(
            format_unknown_command("/foo"),
            "Unknown command '/foo'. Type /h for help."
        );
    }
}
