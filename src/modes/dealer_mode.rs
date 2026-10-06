use bytes::Bytes;
use std::error::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use zeromq::{DealerSocket, Socket, SocketOptions, SocketRecv, SocketSend, ZmqMessage};

use super::sub_mode::bytes_to_display;

/// Resolves the topic frame for DEALER mode.
/// If `topic` is `"*"` or empty `""`, an empty frame is returned.
/// Otherwise, the topic string as bytes.
pub(crate) fn resolve_dealer_topic(topic: &str) -> Bytes {
    if topic == "*" || topic.is_empty() {
        Bytes::new()
    } else {
        Bytes::copy_from_slice(topic.as_bytes())
    }
}

/// Builds a 2-frame Dealer message `[topic, payload]`.
pub(crate) fn build_dealer_message(topic: &Bytes, payload: Bytes) -> ZmqMessage {
    let mut msg = ZmqMessage::from(topic.clone());
    msg.push_back(payload);
    msg
}

/// Parses frames received by DEALER:
/// - 2+ frames: frame 0 is topic, frame 1 is payload (extra frames ignored).
/// - 1 frame: empty topic, frame 0 is payload.
/// - 0 frames: both empty.
pub(crate) fn parse_dealer_frames(msg: &ZmqMessage) -> (String, String) {
    match msg.len() {
        0 => ("".to_string(), "".to_string()),
        1 => {
            let payload_disp = bytes_to_display(msg.get(0).unwrap());
            ("".to_string(), payload_disp)
        }
        _ => {
            let topic_disp = bytes_to_display(msg.get(0).unwrap());
            let payload_disp = bytes_to_display(msg.get(1).unwrap());
            (topic_disp, payload_disp)
        }
    }
}

/// Console line format for Dealer: `[{seq}] {topic} => {payload}`.
pub(crate) fn format_dealer_line(seq: usize, topic: &str, payload: &str) -> String {
    format!("[{}] {} => {}", seq, topic, payload)
}

/// Sets up a DEALER socket, connects or binds, sends lines from stdin, and receives incoming messages concurrently.
pub async fn run_dealer(args: crate::ZmqArgs) -> Result<(), Box<dyn Error>> {
    let mut socket = if let Some(ref identity) = args.identity {
        let mut opts = SocketOptions::default();
        opts.peer_identity(Bytes::copy_from_slice(identity.as_bytes()).try_into()?);
        DealerSocket::with_options(opts)
    } else {
        DealerSocket::new()
    };

    let endpoint = &args.endpoint;
    if args.bind {
        socket.bind(endpoint).await?;
        println!("DEALER bound to {}", endpoint);
    } else {
        socket.connect(endpoint).await?;
        println!("DEALER connected to {}", endpoint);
    }

    let topic_bytes = resolve_dealer_topic(&args.topic);
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    println!("Enter messages to send (Ctrl+C to exit):");

    let mut stdin_active = true;
    let mut recv_count: usize = 0;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nShutdown requested, exiting dealer.");
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
                        let msg = build_dealer_message(&topic_bytes, Bytes::from(line));
                        socket.send(msg).await?;
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
                        let (topic_disp, payload_disp) = parse_dealer_frames(&msg);
                        println!("{}", format_dealer_line(recv_count, &topic_disp, &payload_disp));
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

    // AC-DLR-01: topic defaults to empty frame if * or ""; otherwise specified topic string. Always 2 frames.
    #[test]
    fn ac_dlr_01_topic_resolution_and_message_building() {
        assert_eq!(resolve_dealer_topic("*"), Bytes::new());
        assert_eq!(resolve_dealer_topic(""), Bytes::new());
        assert_eq!(resolve_dealer_topic("chat"), Bytes::from_static(b"chat"));

        let msg_default = build_dealer_message(&resolve_dealer_topic("*"), Bytes::from_static(b"hello"));
        assert_eq!(msg_default.len(), 2);
        assert_eq!(msg_default.get(0).unwrap().as_ref(), b"");
        assert_eq!(msg_default.get(1).unwrap().as_ref(), b"hello");

        let msg_custom = build_dealer_message(&resolve_dealer_topic("news"), Bytes::from_static(b"world"));
        assert_eq!(msg_custom.len(), 2);
        assert_eq!(msg_custom.get(0).unwrap().as_ref(), b"news");
        assert_eq!(msg_custom.get(1).unwrap().as_ref(), b"world");
    }

    // AC-DLR-02: received messages parsing and formatting.
    #[test]
    fn ac_dlr_02_parse_dealer_frames_and_format_line() {
        let mut msg_2 = ZmqMessage::from(Bytes::from_static(b"topic1"));
        msg_2.push_back(Bytes::from_static(b"payload1"));
        let (t, p) = parse_dealer_frames(&msg_2);
        assert_eq!(t, "topic1");
        assert_eq!(p, "payload1");
        assert_eq!(format_dealer_line(1, &t, &p), "[1] topic1 => payload1");

        let mut msg_empty_topic = ZmqMessage::from(Bytes::new());
        msg_empty_topic.push_back(Bytes::from_static(b"ACK"));
        let (t, p) = parse_dealer_frames(&msg_empty_topic);
        assert_eq!(t, "");
        assert_eq!(p, "ACK");
        assert_eq!(format_dealer_line(2, &t, &p), "[2]  => ACK");

        let msg_1 = ZmqMessage::from(Bytes::from_static(b"only_payload"));
        let (t, p) = parse_dealer_frames(&msg_1);
        assert_eq!(t, "");
        assert_eq!(p, "only_payload");
    }
}
