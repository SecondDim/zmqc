use bytes::Bytes;
use std::error::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use zeromq::{RouterSocket, Socket, SocketOptions, SocketRecv, SocketSend, ZmqMessage};

use super::dealer_mode::resolve_dealer_topic;
use super::sub_mode::bytes_to_display;

/// Parses router stdin line: `<message>|<identity>` splitting by the LAST `|`.
/// If no `|` exists or identity part is empty, target is `None` (routes to most recent peer).
pub(crate) fn parse_router_input(input: &str) -> (String, Option<String>) {
    if let Some(idx) = input.rfind('|') {
        let msg = input[..idx].to_string();
        let target = input[idx + 1..].trim().to_string();
        if target.is_empty() {
            (msg, None)
        } else {
            (msg, Some(target))
        }
    } else {
        (input.to_string(), None)
    }
}

/// Tracks known peers and the most recent peer for ROUTER mode.
#[derive(Default, Debug)]
pub(crate) struct PeerRegistry {
    peers: Vec<Bytes>,
    last_peer: Option<Bytes>,
}

impl PeerRegistry {
    pub(crate) fn record_peer(&mut self, id: Bytes) {
        self.last_peer = Some(id.clone());
        if !self.peers.iter().any(|p| p == &id) {
            self.peers.push(id);
        }
    }

    pub(crate) fn last_peer(&self) -> Option<Bytes> {
        self.last_peer.clone()
    }

    /// Looks up a peer identity by text display first, then by lowercase hex match.
    pub(crate) fn find_peer(&self, query: &str) -> Option<Bytes> {
        // 1. Text display match
        for p in &self.peers {
            if bytes_to_display(p) == query {
                return Some(p.clone());
            }
        }
        // 2. Hex match
        let query_lower = query.to_lowercase();
        for p in &self.peers {
            let hex_str: String = p.iter().map(|b| format!("{:02x}", b)).collect();
            if hex_str == query_lower {
                return Some(p.clone());
            }
        }
        None
    }
}

/// Builds a 3-frame Router message `[identity, topic, payload]`.
pub(crate) fn build_router_message(target_identity: Bytes, topic: &Bytes, payload: Bytes) -> ZmqMessage {
    let mut msg = ZmqMessage::from(target_identity);
    msg.push_back(topic.clone());
    msg.push_back(payload);
    msg
}

/// Builds an automatic ACK message `[identity, empty_topic, "ACK"]`.
pub(crate) fn build_ack_message(target_identity: Bytes) -> ZmqMessage {
    build_router_message(target_identity, &Bytes::new(), Bytes::from_static(b"ACK"))
}

/// Parses frames received by ROUTER:
/// - 3+ frames: frame 0 is identity, frame 1 topic, frame 2 payload (extra ignored).
/// - 2 frames: frame 0 is identity, empty topic, frame 1 payload.
/// - 1 frame: frame 0 is identity, empty topic, empty payload.
/// - 0 frames: all empty.
pub(crate) fn parse_router_frames(msg: &ZmqMessage) -> (Bytes, String, String, String) {
    match msg.len() {
        0 => (Bytes::new(), "".to_string(), "".to_string(), "".to_string()),
        1 => {
            let raw_id = msg.get(0).unwrap().clone();
            let id_disp = bytes_to_display(&raw_id);
            (raw_id, id_disp, "".to_string(), "".to_string())
        }
        2 => {
            let raw_id = msg.get(0).unwrap().clone();
            let id_disp = bytes_to_display(&raw_id);
            let payload_disp = bytes_to_display(msg.get(1).unwrap());
            (raw_id, id_disp, "".to_string(), payload_disp)
        }
        _ => {
            let raw_id = msg.get(0).unwrap().clone();
            let id_disp = bytes_to_display(&raw_id);
            let topic_disp = bytes_to_display(msg.get(1).unwrap());
            let payload_disp = bytes_to_display(msg.get(2).unwrap());
            (raw_id, id_disp, topic_disp, payload_disp)
        }
    }
}

/// Console line format for Router: `[{seq}] {identity} {topic} => {payload}`.
pub(crate) fn format_router_line(seq: usize, identity: &str, topic: &str, payload: &str) -> String {
    format!("[{}] {} {} => {}", seq, identity, topic, payload)
}

/// Sets up a ROUTER socket, always binds, handles incoming messages (with optional --ack),
/// and accepts messages from stdin to send to peers.
pub async fn run_router(args: crate::ZmqArgs) -> Result<(), Box<dyn Error>> {
    let mut socket = if let Some(ref identity) = args.identity {
        let mut opts = SocketOptions::default();
        opts.peer_identity(Bytes::copy_from_slice(identity.as_bytes()).try_into()?);
        RouterSocket::with_options(opts)
    } else {
        RouterSocket::new()
    };

    let endpoint = &args.endpoint;
    // Router always binds, ignoring args.bind setting
    socket.bind(endpoint).await?;
    println!("ROUTER bound to {}", endpoint);

    let topic_bytes = resolve_dealer_topic(&args.topic);
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    println!("Enter messages to send (or '<message>|<identity>', Ctrl+C to exit):");

    let mut peer_registry = PeerRegistry::default();
    let mut stdin_active = true;
    let mut recv_count: usize = 0;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nShutdown requested, exiting router.");
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
                    Some(line) => {
                        let (msg_text, target_opt) = parse_router_input(&line);
                        let target_peer = match target_opt {
                            Some(target_name) => peer_registry.find_peer(&target_name),
                            None => peer_registry.last_peer(),
                        };

                        if let Some(target_id) = target_peer {
                            let msg = build_router_message(target_id, &topic_bytes, Bytes::from(msg_text));
                            // Unknown or disconnected peer errors are silently dropped per D9
                            let _ = socket.send(msg).await;
                        }
                    }
                    None => {
                        stdin_active = false;
                        println!("stdin reached EOF, continuing to listen for incoming messages (Ctrl+C to exit)...");
                    }
                }
            }
            recv_res = socket.recv() => {
                match recv_res {
                    Ok(msg) => {
                        recv_count += 1;
                        let (raw_id, id_disp, topic_disp, payload_disp) = parse_router_frames(&msg);
                        peer_registry.record_peer(raw_id.clone());

                        println!("{}", format_router_line(recv_count, &id_disp, &topic_disp, &payload_disp));

                        if args.ack && !raw_id.is_empty() {
                            let ack_msg = build_ack_message(raw_id);
                            let _ = socket.send(ack_msg).await;
                        }
                    }
                    Err(e) => {
                        eprintln!("Socket receive error: {}", e);
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-RTR-01: parse_router_input splits on last |
    #[test]
    fn ac_rtr_01_parse_router_input() {
        let (m1, id1) = parse_router_input("hello|alice");
        assert_eq!(m1, "hello");
        assert_eq!(id1, Some("alice".to_string()));

        let (m2, id2) = parse_router_input("a|b|c|target");
        assert_eq!(m2, "a|b|c");
        assert_eq!(id2, Some("target".to_string()));

        let (m3, id3) = parse_router_input("plain text");
        assert_eq!(m3, "plain text");
        assert_eq!(id3, None);

        let (m4, id4) = parse_router_input("trailing pipe|");
        assert_eq!(m4, "trailing pipe");
        assert_eq!(id4, None);
    }

    // AC-RTR-02: parse_router_frames and format_router_line
    #[test]
    fn ac_rtr_02_parse_router_frames_and_format_line() {
        let mut msg_3 = ZmqMessage::from(Bytes::from_static(b"client-a"));
        msg_3.push_back(Bytes::from_static(b"chat"));
        msg_3.push_back(Bytes::from_static(b"hello"));
        let (raw, id, topic, payload) = parse_router_frames(&msg_3);
        assert_eq!(raw.as_ref(), b"client-a");
        assert_eq!(id, "client-a");
        assert_eq!(topic, "chat");
        assert_eq!(payload, "hello");
        assert_eq!(format_router_line(1, &id, &topic, &payload), "[1] client-a chat => hello");

        // 2 frames: identity + payload, empty topic
        let mut msg_2 = ZmqMessage::from(Bytes::from_static(b"client-b"));
        msg_2.push_back(Bytes::from_static(b"hello2"));
        let (_, id2, topic2, payload2) = parse_router_frames(&msg_2);
        assert_eq!(id2, "client-b");
        assert_eq!(topic2, "");
        assert_eq!(payload2, "hello2");
        assert_eq!(format_router_line(2, &id2, &topic2, &payload2), "[2] client-b  => hello2");

        // Non-UTF8 identity falls back to lowercase hex
        let mut msg_hex = ZmqMessage::from(Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]));
        msg_hex.push_back(Bytes::from_static(b"t"));
        msg_hex.push_back(Bytes::from_static(b"p"));
        let (_, id_hex, _, _) = parse_router_frames(&msg_hex);
        assert_eq!(id_hex, "deadbeef");
    }

    // AC-RTR-03: build_router_message creates 3 frames
    #[test]
    fn ac_rtr_03_build_router_message() {
        let msg = build_router_message(Bytes::from_static(b"peer1"), &Bytes::from_static(b"top"), Bytes::from_static(b"pay"));
        assert_eq!(msg.len(), 3);
        assert_eq!(msg.get(0).unwrap().as_ref(), b"peer1");
        assert_eq!(msg.get(1).unwrap().as_ref(), b"top");
        assert_eq!(msg.get(2).unwrap().as_ref(), b"pay");
    }

    // AC-RTR-04 & AC-RTR-05: PeerRegistry lookup and default to last peer
    #[test]
    fn ac_rtr_04_and_05_peer_registry() {
        let mut reg = PeerRegistry::default();
        assert_eq!(reg.last_peer(), None);
        assert_eq!(reg.find_peer("alice"), None);

        let alice_bytes = Bytes::from_static(b"alice");
        reg.record_peer(alice_bytes.clone());
        assert_eq!(reg.last_peer(), Some(alice_bytes.clone()));
        assert_eq!(reg.find_peer("alice"), Some(alice_bytes.clone()));

        let hex_peer = Bytes::from_static(&[0x0a, 0x0b]);
        reg.record_peer(hex_peer.clone());
        assert_eq!(reg.last_peer(), Some(hex_peer.clone()));
        // Lookup by hex string
        assert_eq!(reg.find_peer("0a0b"), Some(hex_peer.clone()));
        assert_eq!(reg.find_peer("0A0B"), Some(hex_peer.clone()));
        // Still can find alice
        assert_eq!(reg.find_peer("alice"), Some(alice_bytes));
        // Non-existent peer
        assert_eq!(reg.find_peer("unknown"), None);
    }

    // AC-RTR-06: build_ack_message
    #[test]
    fn ac_rtr_06_build_ack_message() {
        let ack = build_ack_message(Bytes::from_static(b"client-x"));
        assert_eq!(ack.len(), 3);
        assert_eq!(ack.get(0).unwrap().as_ref(), b"client-x");
        assert_eq!(ack.get(1).unwrap().as_ref(), b"");
        assert_eq!(ack.get(2).unwrap().as_ref(), b"ACK");
    }
}
