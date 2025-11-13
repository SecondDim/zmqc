use std::error::Error;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use zeromq::{Socket, SocketRecv, SubSocket, ZmqMessage};

/// How often the background file writer flushes buffered lines to disk.
pub(crate) const FILE_FLUSH_INTERVAL: Duration = Duration::from_secs(3);

/// Capacity of the queue between the network receive loop and the file writer.
const FILE_QUEUE_CAPACITY: usize = 10_000;

/// Sets up a SUB socket, binds or connects, subscribes to a topic (or all), and receives incoming messages.
pub async fn run_subscriber(args: crate::ZmqArgs) -> Result<(), Box<dyn Error>> {
    let mut socket = SubSocket::new();
    let endpoint = &args.endpoint;

    if args.bind {
        socket.bind(endpoint).await?;
        println!("SUB bound to {}", endpoint);
    } else {
        socket.connect(endpoint).await?;
        println!("SUB connected to {}", endpoint);
    }

    let effective_topic = resolve_sub_topic(&args.topic);
    if effective_topic.is_empty() {
        socket.subscribe("").await?;
        println!("Subscribed to all topics");
    } else {
        socket.subscribe(effective_topic).await?;
        println!("Subscribed to topic: {}", effective_topic);
    }

    println!("Waiting for messages...");

    // Setup file output worker if requested
    let (file_tx, file_task_handle) = if let Some(output_file) = args.file {
        let path = std::path::Path::new(&output_file);
        if path.exists() {
            println!(
                "Output file '{}' already exists. Overwrite? (y/N):",
                output_file
            );
            let mut ans = String::new();
            let mut stdin_reader = BufReader::new(tokio::io::stdin());
            stdin_reader.read_line(&mut ans).await?;
            if !is_overwrite_confirmed(&ans) {
                println!("Aborted by user.");
                return Ok(());
            }
        }

        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&output_file)
            .await?;

        let (tx, handle) = spawn_file_writer(file, FILE_FLUSH_INTERVAL);
        (Some(tx), Some(handle))
    } else {
        (None, None)
    };

    let mut recv_count: usize = 0;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nShutdown requested, exiting subscriber.");
                break;
            }
            recv_result = socket.recv() => {
                match recv_result {
                    Ok(msg) => {
                        recv_count += 1;
                        let (topic_disp, msg_disp) = parse_message_frames(&msg);
                        let formatted = format_message_line(recv_count, &topic_disp, &msg_disp);

                        if let Some(ref tx) = file_tx {
                            if tx.send(formatted).await.is_err() {
                                eprintln!("File writer task ended unexpectedly.");
                                break;
                            }
                        } else {
                            println!("{}", formatted);
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

    // Graceful shutdown: drop sender and wait for file writer task to flush and complete
    if let Some(tx) = file_tx {
        drop(tx);
        if let Some(handle) = file_task_handle {
            let _ = handle.await;
            println!("File writer flushed and closed successfully.");
        }
    }

    Ok(())
}

/// In SUB mode, if `topic` starts with `'*'`, it maps to `""` (receives all data).
/// Otherwise, returns `topic` as-is.
pub(crate) fn resolve_sub_topic(topic: &str) -> &str {
    if topic.starts_with('*') {
        ""
    } else {
        topic
    }
}

/// Only `y` / `yes` (case-insensitive, surrounding whitespace ignored) confirm an overwrite.
pub(crate) fn is_overwrite_confirmed(answer: &str) -> bool {
    let a = answer.trim().to_lowercase();
    a == "y" || a == "yes"
}

/// Console / file line format: `[{seq}] {topic} => {payload}`.
pub(crate) fn format_message_line(seq: usize, topic: &str, payload: &str) -> String {
    format!("[{}] {} => {}", seq, topic, payload)
}

/// Spawns the background file writer.
///
/// Lines sent through the returned channel are written newline-terminated, flushed every
/// `flush_every`, and flushed one last time once all senders are dropped (graceful shutdown).
pub(crate) fn spawn_file_writer(
    file: tokio::fs::File,
    flush_every: Duration,
) -> (mpsc::Sender<String>, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<String>(FILE_QUEUE_CAPACITY);
    let handle = tokio::spawn(async move {
        let mut writer = tokio::io::BufWriter::new(file);
        let mut flush_interval = tokio::time::interval(flush_every);
        flush_interval.tick().await;

        loop {
            tokio::select! {
                msg_opt = rx.recv() => {
                    match msg_opt {
                        Some(line) => {
                            let _ = writer.write_all(line.as_bytes()).await;
                            let _ = writer.write_all(b"\n").await;
                        }
                        None => {
                            let _ = writer.flush().await;
                            break;
                        }
                    }
                }
                _ = flush_interval.tick() => {
                    let _ = writer.flush().await;
                }
            }
        }
    });
    (tx, handle)
}

/// Splits a received message into `(topic, payload)` display strings.
///
/// - 2+ frames: frame 0 is the topic, frame 1 the payload (extra frames are ignored).
/// - 1 frame: empty topic, frame 0 is the payload.
/// - 0 frames: both empty.
pub(crate) fn parse_message_frames(msg: &ZmqMessage) -> (String, String) {
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

/// UTF-8 is shown as text; anything else falls back to lowercase hex.
pub(crate) fn bytes_to_display(data: &[u8]) -> String {
    match std::str::from_utf8(data) {
        Ok(s) => s.to_string(),
        Err(_) => data
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::test_util::TempPath;
    use bytes::Bytes;

    fn msg(frames: &[&[u8]]) -> ZmqMessage {
        let mut m = ZmqMessage::from(Bytes::copy_from_slice(frames[0]));
        for f in &frames[1..] {
            m.push_back(Bytes::copy_from_slice(f));
        }
        m
    }

    // ---- AC-SUB-01: display (UTF-8 vs hex) ----

    #[test]
    fn ac_sub_01_utf8_is_displayed_as_text() {
        assert_eq!(bytes_to_display(b"hello"), "hello");
        assert_eq!(bytes_to_display("你好".as_bytes()), "你好");
        assert_eq!(bytes_to_display(b""), "");
    }

    #[test]
    fn ac_sub_01_non_utf8_falls_back_to_lowercase_zero_padded_hex() {
        assert_eq!(bytes_to_display(&[0xff, 0x00, 0x0a, 0xAB]), "ff000aab");
    }

    // ---- AC-SUB-02: frame parsing ----

    #[test]
    fn ac_sub_02_two_frames_are_topic_and_payload() {
        let (t, p) = parse_message_frames(&msg(&[b"sensor", b"42"]));
        assert_eq!((t.as_str(), p.as_str()), ("sensor", "42"));
    }

    #[test]
    fn ac_sub_02_single_frame_has_empty_topic() {
        let (t, p) = parse_message_frames(&msg(&[b"only"]));
        assert_eq!((t.as_str(), p.as_str()), ("", "only"));
    }

    #[test]
    fn ac_sub_02_extra_frames_are_ignored() {
        let (t, p) = parse_message_frames(&msg(&[b"t", b"p", b"extra"]));
        assert_eq!((t.as_str(), p.as_str()), ("t", "p"));
    }

    #[test]
    fn ac_sub_02_binary_payload_is_hex_while_topic_stays_text() {
        let (t, p) = parse_message_frames(&msg(&[b"bin", &[0xff, 0xad]]));
        assert_eq!((t.as_str(), p.as_str()), ("bin", "ffad"));
    }

    // ---- AC-SUB-03: output line format ----

    #[test]
    fn ac_sub_03_line_format_is_seq_topic_arrow_payload() {
        assert_eq!(format_message_line(1, "chat", "hi"), "[1] chat => hi");
        assert_eq!(format_message_line(12, "", "x"), "[12]  => x");
    }

    // ---- AC-SUB-04: overwrite confirmation ----

    #[test]
    fn ac_sub_04_only_y_or_yes_confirm_overwrite() {
        for yes in ["y", "Y", "yes", "YES", "Yes", " y\n", "yes\r\n"] {
            assert!(is_overwrite_confirmed(yes), "{yes:?} should confirm");
        }
        for no in ["", "\n", "n", "N", "no", "yy", "yeah", "ok", "1"] {
            assert!(!is_overwrite_confirmed(no), "{no:?} should not confirm");
        }
    }

    // ---- AC-SUB-05: file writer ----

    async fn open_for_write(path: &TempPath) -> tokio::fs::File {
        tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path.as_str())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn ac_sub_05_lines_are_written_newline_terminated_in_order() {
        let path = TempPath::new("sub_order.log");
        let (tx, handle) =
            spawn_file_writer(open_for_write(&path).await, Duration::from_secs(60));

        tx.send("[1] a => 1".into()).await.unwrap();
        tx.send("[2] b => 2".into()).await.unwrap();
        drop(tx);
        handle.await.unwrap();

        assert_eq!(path.read_to_string(), "[1] a => 1\n[2] b => 2\n");
    }

    #[tokio::test]
    async fn ac_sub_05_output_file_is_truncated_on_open() {
        let path = TempPath::with_bytes("sub_trunc.log", b"OLD CONTENT THAT MUST GO\n");
        let (tx, handle) =
            spawn_file_writer(open_for_write(&path).await, Duration::from_secs(60));
        tx.send("new".into()).await.unwrap();
        drop(tx);
        handle.await.unwrap();

        assert_eq!(path.read_to_string(), "new\n");
    }

    // ---- AC-SUB-06: periodic flush ----

    #[tokio::test]
    async fn ac_sub_06_buffered_lines_are_flushed_periodically_before_shutdown() {
        let path = TempPath::new("sub_periodic.log");
        let (tx, handle) =
            spawn_file_writer(open_for_write(&path).await, Duration::from_millis(50));

        tx.send("tick".into()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Channel still open: data must already be on disk thanks to the periodic flush.
        assert_eq!(path.read_to_string(), "tick\n");

        drop(tx);
        handle.await.unwrap();
    }

    // ---- AC-SUB-07: graceful shutdown drains the queue ----

    #[tokio::test]
    async fn ac_sub_07_closing_the_channel_drains_every_queued_message() {
        let path = TempPath::new("sub_drain.log");
        let (tx, handle) =
            spawn_file_writer(open_for_write(&path).await, Duration::from_secs(60));

        for i in 0..2_000 {
            tx.send(format!("[{i}] t => p")).await.unwrap();
        }
        drop(tx);
        handle.await.unwrap();

        let content = path.read_to_string();
        assert_eq!(content.lines().count(), 2_000);
        assert_eq!(content.lines().last(), Some("[1999] t => p"));
    }

    // ---- AC-SUB-08: wildcard topic resolution ----

    #[test]
    fn ac_sub_08_resolve_sub_topic_wildcards() {
        assert_eq!(resolve_sub_topic("*"), "");
        assert_eq!(resolve_sub_topic("*all"), "");
        assert_eq!(resolve_sub_topic("*sensor"), "");
        assert_eq!(resolve_sub_topic(""), "");
        assert_eq!(resolve_sub_topic("sensor"), "sensor");
        assert_eq!(resolve_sub_topic("chat.general"), "chat.general");
    }
}
