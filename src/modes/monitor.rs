use futures_channel::mpsc::Receiver;
use zeromq::SocketEvent;

use super::sub_mode::bytes_to_display;

/// Formats a `SocketEvent` into a console output message.
///
/// Core events (`Accepted`, `Connected`, `Disconnected`, `AcceptFailed`) are always formatted.
/// Internal events (`Listening`, `ConnectDelayed`, `ConnectRetried`, `Closed`, `CloseFailed`)
/// are only formatted when `debug` is `true`; otherwise `None` is returned.
pub(crate) fn format_socket_event(
    event: &SocketEvent,
    prefix: Option<&str>,
    debug: bool,
) -> Option<String> {
    let tag = match prefix {
        Some(p) => format!("[PEER] {}", p),
        None => "[PEER]".to_string(),
    };

    match event {
        SocketEvent::Accepted(endpoint, peer_id) => {
            let id_disp = bytes_to_display(peer_id.as_ref());
            Some(format!("{} Connected: {} ({})", tag, endpoint, id_disp))
        }
        SocketEvent::Connected(endpoint, peer_id) => {
            let id_disp = bytes_to_display(peer_id.as_ref());
            Some(format!("{} Connected: {} ({})", tag, endpoint, id_disp))
        }
        SocketEvent::Disconnected(peer_id) => {
            let id_disp = bytes_to_display(peer_id.as_ref());
            Some(format!("{} Disconnected: {}", tag, id_disp))
        }
        SocketEvent::AcceptFailed(err) => {
            Some(format!("{} Accept failed: {}", tag, err))
        }
        SocketEvent::Listening(endpoint) => {
            if debug {
                Some(format!("{} Listening on {}", tag, endpoint))
            } else {
                None
            }
        }
        SocketEvent::ConnectDelayed => {
            if debug {
                Some(format!("{} Connect delayed", tag))
            } else {
                None
            }
        }
        SocketEvent::ConnectRetried => {
            if debug {
                Some(format!("{} Connect retried", tag))
            } else {
                None
            }
        }
        SocketEvent::Closed => {
            if debug {
                Some(format!("{} Closed", tag))
            } else {
                None
            }
        }
        SocketEvent::CloseFailed => {
            if debug {
                Some(format!("{} Close failed", tag))
            } else {
                None
            }
        }
    }
}

/// Spawns a background task that listens for `SocketEvent`s and prints formatted messages.
pub(crate) fn spawn_socket_monitor(
    mut receiver: Receiver<SocketEvent>,
    prefix: Option<&'static str>,
    debug: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use futures_util::StreamExt;
        while let Some(event) = receiver.next().await {
            if let Some(msg) = format_socket_event(&event, prefix, debug) {
                println!("{}", msg);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use zeromq::util::PeerIdentity;
    use zeromq::Endpoint;

    fn test_peer_id(raw: &[u8]) -> PeerIdentity {
        Bytes::copy_from_slice(raw).try_into().unwrap()
    }

    fn test_endpoint(addr: &str) -> Endpoint {
        use std::str::FromStr;
        Endpoint::from_str(addr).unwrap()
    }

    // AC-MON-01: format_socket_event formats Accepted and Connected
    #[test]
    fn ac_mon_01_accepted_and_connected() {
        let ep = test_endpoint("tcp://127.0.0.1:5555");
        let id = test_peer_id(b"alice");

        let acc = SocketEvent::Accepted(ep.clone(), id.clone());
        assert_eq!(
            format_socket_event(&acc, None, false),
            Some("[PEER] Connected: tcp://127.0.0.1:5555 (alice)".to_string())
        );

        let conn = SocketEvent::Connected(ep, id);
        assert_eq!(
            format_socket_event(&conn, None, false),
            Some("[PEER] Connected: tcp://127.0.0.1:5555 (alice)".to_string())
        );

        // Non-UTF8 peer identity displays as hex
        let hex_id = test_peer_id(&[0xde, 0xad, 0xbe, 0xef]);
        let conn_hex = SocketEvent::Connected(test_endpoint("tcp://127.0.0.1:5555"), hex_id);
        assert_eq!(
            format_socket_event(&conn_hex, None, false),
            Some("[PEER] Connected: tcp://127.0.0.1:5555 (deadbeef)".to_string())
        );
    }

    // AC-MON-02: format_socket_event formats Disconnected
    #[test]
    fn ac_mon_02_disconnected() {
        let id = test_peer_id(b"bob");
        let dis = SocketEvent::Disconnected(id);
        assert_eq!(
            format_socket_event(&dis, None, false),
            Some("[PEER] Disconnected: bob".to_string())
        );

        let hex_id = test_peer_id(&[0xff, 0xfe]);
        let dis_hex = SocketEvent::Disconnected(hex_id);
        assert_eq!(
            format_socket_event(&dis_hex, None, false),
            Some("[PEER] Disconnected: fffe".to_string())
        );
    }

    // AC-MON-03: format_socket_event formats AcceptFailed
    #[test]
    fn ac_mon_03_accept_failed() {
        let err = zeromq::ZmqError::Other("network timeout");
        let ev = SocketEvent::AcceptFailed(err);
        assert_eq!(
            format_socket_event(&ev, None, false),
            Some("[PEER] Accept failed: network timeout".to_string())
        );
    }

    // AC-MON-04: format_socket_event with role prefix (e.g. Proxy mode [XPUB] / [XSUB])
    #[test]
    fn ac_mon_04_role_prefix() {
        let ep = test_endpoint("tcp://127.0.0.1:5556");
        let id = test_peer_id(b"client1");

        let acc_xpub = SocketEvent::Accepted(ep.clone(), id.clone());
        assert_eq!(
            format_socket_event(&acc_xpub, Some("[XPUB]"), false),
            Some("[PEER] [XPUB] Connected: tcp://127.0.0.1:5556 (client1)".to_string())
        );

        let dis_xsub = SocketEvent::Disconnected(id);
        assert_eq!(
            format_socket_event(&dis_xsub, Some("[XSUB]"), false),
            Some("[PEER] [XSUB] Disconnected: client1".to_string())
        );
    }

    // AC-MON-05: internal events with and without debug
    #[test]
    fn ac_mon_05_internal_events_with_and_without_debug() {
        let ep = test_endpoint("tcp://127.0.0.1:5555");
        let listening = SocketEvent::Listening(ep);
        assert_eq!(format_socket_event(&listening, None, false), None);
        assert_eq!(
            format_socket_event(&listening, None, true),
            Some("[PEER] Listening on tcp://127.0.0.1:5555".to_string())
        );

        let delayed = SocketEvent::ConnectDelayed;
        assert_eq!(format_socket_event(&delayed, None, false), None);
        assert_eq!(
            format_socket_event(&delayed, None, true),
            Some("[PEER] Connect delayed".to_string())
        );

        let retried = SocketEvent::ConnectRetried;
        assert_eq!(format_socket_event(&retried, None, false), None);
        assert_eq!(
            format_socket_event(&retried, None, true),
            Some("[PEER] Connect retried".to_string())
        );

        let closed = SocketEvent::Closed;
        assert_eq!(format_socket_event(&closed, None, false), None);
        assert_eq!(
            format_socket_event(&closed, None, true),
            Some("[PEER] Closed".to_string())
        );

        let close_failed = SocketEvent::CloseFailed;
        assert_eq!(format_socket_event(&close_failed, None, false), None);
        assert_eq!(
            format_socket_event(&close_failed, None, true),
            Some("[PEER] Close failed".to_string())
        );
    }
}
