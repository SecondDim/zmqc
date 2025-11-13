use bytes::Bytes;
use std::error::Error;
use std::io::SeekFrom;
use std::time::Duration;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncSeekExt, BufReader,
};
use zeromq::{PubSocket, Socket, SocketSend, ZmqMessage};

pub(crate) type BoxResult<T> = Result<T, Box<dyn Error>>;

/// Number of leading bytes inspected to decide whether `--file` is plain text.
const PROBE_SIZE: usize = 1024;

/// Delay after bind/connect so that subscribers can finish their handshake.
const SLOW_JOINER_DELAY: Duration = Duration::from_millis(500);

/// Anything that can publish a `[topic, payload]` two-frame message.
///
/// Abstracting the socket lets the streaming logic be unit-tested without a network.
pub(crate) trait TwoPartSender {
    async fn send_two_part(&mut self, topic: &Bytes, payload: Bytes) -> BoxResult<()>;
}

impl TwoPartSender for PubSocket {
    async fn send_two_part(&mut self, topic: &Bytes, payload: Bytes) -> BoxResult<()> {
        let mut msg = ZmqMessage::from(topic.clone());
        msg.push_back(payload);
        self.send(msg).await?;
        Ok(())
    }
}

/// Sets up a PUB socket, binds or connects, and publishes lines from file or stdin.
pub async fn run_publisher(args: crate::ZmqArgs) -> Result<(), Box<dyn Error>> {
    let mut socket = PubSocket::new();
    let endpoint = &args.endpoint;

    if args.bind {
        socket.bind(endpoint).await?;
        println!("PUB bound to {}", endpoint);
    } else {
        socket.connect(endpoint).await?;
        println!("PUB connected to {}", endpoint);
    }

    // Slow joiner mitigation
    tokio::time::sleep(SLOW_JOINER_DELAY).await;

    let topic_str = match validate_pub_topic(&args.topic) {
        Ok(valid_topic) => valid_topic.to_string(),
        Err(_) if args.topic.trim() == "*" => {
            println!("Enter topic to publish (Ctrl+C to exit):");
            let mut topic_input = String::new();
            let mut stdin_reader = BufReader::new(tokio::io::stdin());
            let n = stdin_reader.read_line(&mut topic_input).await?;
            if n == 0 {
                return Err("PUB mode does not allow topic to be empty or '*'".into());
            }
            validate_pub_topic(&topic_input)?.to_string()
        }
        Err(e) => return Err(e.into()),
    };
    let topic_bytes = Bytes::copy_from_slice(topic_str.as_bytes());

    if let Some(input_file) = args.file {
        let mut throttle = Throttle::new(args.batch_size, args.throttle_interval_ms);
        publish_file(&input_file, &topic_bytes, &mut socket, &mut throttle).await?;
    }

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    println!("Enter messages to publish (Ctrl+C to exit):");

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nShutdown requested, exiting publisher.");
                break;
            }
            line_res = reader.next_line() => {
                match line_res? {
                    Some(message) => {
                        socket.send_two_part(&topic_bytes, Bytes::from(message)).await?;
                    }
                    None => {
                        // EOF reached
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Publishes the content of `path`, auto-detecting UTF-8 text vs. the custom binary format.
pub(crate) async fn publish_file<S: TwoPartSender>(
    path: &str,
    topic: &Bytes,
    sender: &mut S,
    throttle: &mut Throttle,
) -> BoxResult<()> {
    let mut file = tokio::fs::File::open(path).await?;

    // Probe the first bytes to check if UTF-8 plain text
    let mut probe_buf = [0u8; PROBE_SIZE];
    let n = file.read(&mut probe_buf).await.unwrap_or(0);
    file.seek(SeekFrom::Start(0)).await?;

    if is_utf8_text(&probe_buf[..n], n == PROBE_SIZE) {
        stream_text_lines(BufReader::new(file), topic, sender).await?;
    } else {
        let mut reader = BufReader::new(file);
        let summary = stream_binary_packets(&mut reader, topic, sender, throttle).await?;
        println!(
            "{} => {:?}",
            summary.total_count,
            String::from_utf8(summary.last_payload)
        );
    }
    Ok(())
}

/// Validates the topic for PUB mode.
/// In PUB mode, `topic` is NOT allowed to be `""` or `"*"` (trimmed).
pub(crate) fn validate_pub_topic(topic: &str) -> Result<&str, &'static str> {
    let trimmed = topic.trim();
    if trimmed.is_empty() || trimmed == "*" {
        Err("PUB mode does not allow topic to be empty or '*'")
    } else {
        Ok(trimmed)
    }
}

/// Returns true when `probe` looks like UTF-8 text.
///
/// When `may_be_truncated` is set (the probe filled the whole probe buffer), a multi-byte
/// character cut off at the very end is not treated as evidence of binary content.
pub(crate) fn is_utf8_text(probe: &[u8], may_be_truncated: bool) -> bool {
    match std::str::from_utf8(probe) {
        Ok(_) => true,
        Err(e) => may_be_truncated && e.error_len().is_none(),
    }
}

/// Sends every non-empty line of `reader` as `[topic, line]`. Returns the number of messages sent.
pub(crate) async fn stream_text_lines<R, S>(
    reader: R,
    topic: &Bytes,
    sender: &mut S,
) -> BoxResult<usize>
where
    R: AsyncBufRead + Unpin,
    S: TwoPartSender,
{
    let mut lines = reader.lines();
    let mut sent = 0;
    while let Some(line) = lines.next_line().await? {
        if !line.is_empty() {
            sender.send_two_part(topic, Bytes::from(line)).await?;
            sent += 1;
        }
    }
    Ok(sent)
}

/// Result of streaming a binary file.
pub(crate) struct BinaryStreamSummary {
    pub total_count: usize,
    pub last_payload: Vec<u8>,
}

/// Streams the custom binary format.
///
/// Header (9 bytes): `active`(1) + `seq_lock`(i32 LE) + `data_offset`(i32 LE).
/// Body, repeated until the byte offset reaches `data_offset`:
/// `data_len`(i32 LE) + `data`(data_len bytes) + `bin_times`(8 bytes).
pub(crate) async fn stream_binary_packets<R, S>(
    reader: &mut R,
    topic: &Bytes,
    sender: &mut S,
    throttle: &mut Throttle,
) -> BoxResult<BinaryStreamSummary>
where
    R: AsyncRead + Unpin,
    S: TwoPartSender,
{
    let mut active_buf = [0u8; 1];
    let _ = match reader.read_exact(&mut active_buf).await {
        Ok(_) => active_buf[0] != 0,
        _ => false,
    };

    let mut seq_lock_buf = [0u8; 4];
    let _ = match reader.read_exact(&mut seq_lock_buf).await {
        Ok(_) => i32::from_le_bytes(seq_lock_buf),
        _ => 0,
    };

    let mut data_offset_buf = [0u8; 4];
    let data_offset = match reader.read_exact(&mut data_offset_buf).await {
        Ok(_) => i32::from_le_bytes(data_offset_buf),
        _ => 0,
    } as usize;

    let mut offset = active_buf.len() + seq_lock_buf.len() + data_offset_buf.len();
    let mut total_count = 0;
    let mut last_payload = Vec::new();
    loop {
        if offset >= data_offset {
            break;
        }

        let mut data_len_buf = [0u8; 4];
        let data_len = match reader.read_exact(&mut data_len_buf).await {
            Ok(_) => i32::from_le_bytes(data_len_buf),
            _ => 0,
        } as usize;

        let mut data_buf = vec![0u8; data_len];
        let mut pause = None;
        if let Ok(_) = reader.read_exact(&mut data_buf).await {
            sender
                .send_two_part(topic, Bytes::copy_from_slice(&data_buf))
                .await?;
            total_count += 1;
            pause = throttle.record_send();
        };
        last_payload = data_buf;

        let mut bin_times_buf = [0u8; 8];
        let _ = reader.read_exact(&mut bin_times_buf).await;

        offset = offset + 4 + data_len + 8;

        if let Some(duration) = pause {
            tokio::time::sleep(duration).await;
        }
    }

    Ok(BinaryStreamSummary {
        total_count,
        last_payload,
    })
}

/// Batch throttling for binary streaming: after `batch_size` messages, pause `interval`.
pub(crate) struct Throttle {
    batch_size: usize,
    interval: Duration,
    sent: usize,
}

impl Throttle {
    pub(crate) fn new(batch_size: usize, interval_ms: u64) -> Self {
        Self {
            batch_size,
            interval: Duration::from_millis(interval_ms),
            sent: 0,
        }
    }

    /// Records one sent message. Returns the pause to apply when a batch has just completed.
    /// `batch_size == 0` or an interval of 0 disables pausing.
    pub(crate) fn record_send(&mut self) -> Option<Duration> {
        self.sent += 1;
        if self.batch_size > 0 && self.sent >= self.batch_size {
            self.sent = 0;
            if !self.interval.is_zero() {
                return Some(self.interval);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::test_util::TempPath;
    use std::time::Instant;

    /// Records everything it is asked to send.
    #[derive(Default)]
    struct MockSender {
        sent: Vec<(Vec<u8>, Vec<u8>)>,
    }

    impl TwoPartSender for MockSender {
        async fn send_two_part(&mut self, topic: &Bytes, payload: Bytes) -> BoxResult<()> {
            self.sent.push((topic.to_vec(), payload.to_vec()));
            Ok(())
        }
    }

    fn topic() -> Bytes {
        Bytes::from_static(b"sensor")
    }

    /// Builds a file in the custom binary format from the given packets.
    fn build_binary(packets: &[&[u8]]) -> Vec<u8> {
        let body_len: usize = packets.iter().map(|p| 4 + p.len() + 8).sum();
        let data_offset = (9 + body_len) as i32;
        let mut out = vec![1u8]; // active
        out.extend_from_slice(&7i32.to_le_bytes()); // seq_lock
        out.extend_from_slice(&data_offset.to_le_bytes());
        for p in packets {
            out.extend_from_slice(&(p.len() as i32).to_le_bytes());
            out.extend_from_slice(p);
            out.extend_from_slice(&[0xAA; 8]); // bin_times
        }
        out
    }

    // ---- AC-PUB-01: UTF-8 text detection ----

    #[test]
    fn ac_pub_01_ascii_and_cjk_are_text() {
        assert!(is_utf8_text(b"hello\nworld", false));
        assert!(is_utf8_text("你好，世界".as_bytes(), false));
        assert!(is_utf8_text(b"", false));
    }

    #[test]
    fn ac_pub_01_invalid_bytes_are_binary() {
        assert!(!is_utf8_text(&[0xff, 0xfe, 0x00, 0x01], false));
        assert!(!is_utf8_text(&[0xff, 0xfe], true));
    }

    #[test]
    fn ac_pub_01_multibyte_cut_at_probe_boundary_is_still_text() {
        let cut = &"你".as_bytes()[..2]; // incomplete trailing character
        assert!(is_utf8_text(cut, true));
        assert!(!is_utf8_text(cut, false));
    }

    // ---- AC-PUB-02: text streaming ----

    #[tokio::test]
    async fn ac_pub_02_text_lines_are_sent_in_order_skipping_empty() {
        let mut sender = MockSender::default();
        let input = b"first\n\nsecond\n\n\nthird";
        let sent = stream_text_lines(&input[..], &topic(), &mut sender)
            .await
            .unwrap();

        assert_eq!(sent, 3);
        let payloads: Vec<_> = sender.sent.iter().map(|(_, p)| p.clone()).collect();
        assert_eq!(
            payloads,
            vec![b"first".to_vec(), b"second".to_vec(), b"third".to_vec()]
        );
        assert!(sender.sent.iter().all(|(t, _)| t == b"sensor"));
    }

    #[tokio::test]
    async fn ac_pub_02_empty_input_sends_nothing() {
        let mut sender = MockSender::default();
        let sent = stream_text_lines(&b""[..], &topic(), &mut sender)
            .await
            .unwrap();
        assert_eq!(sent, 0);
        assert!(sender.sent.is_empty());
    }

    // ---- AC-PUB-03: binary streaming ----

    #[tokio::test]
    async fn ac_pub_03_binary_packets_are_sent_in_order_with_topic() {
        let file = build_binary(&[b"alpha", b"be", &[0xde, 0xad, 0xbe, 0xef]]);
        let mut sender = MockSender::default();
        let mut throttle = Throttle::new(0, 0);
        let mut reader = &file[..];

        let summary = stream_binary_packets(&mut reader, &topic(), &mut sender, &mut throttle)
            .await
            .unwrap();

        assert_eq!(summary.total_count, 3);
        assert_eq!(summary.last_payload, vec![0xde, 0xad, 0xbe, 0xef]);
        let payloads: Vec<_> = sender.sent.iter().map(|(_, p)| p.clone()).collect();
        assert_eq!(
            payloads,
            vec![b"alpha".to_vec(), b"be".to_vec(), vec![0xde, 0xad, 0xbe, 0xef]]
        );
        assert!(sender.sent.iter().all(|(t, _)| t == b"sensor"));
    }

    #[tokio::test]
    async fn ac_pub_03_header_only_binary_sends_nothing() {
        let file = build_binary(&[]);
        let mut sender = MockSender::default();
        let mut throttle = Throttle::new(0, 0);
        let mut reader = &file[..];

        let summary = stream_binary_packets(&mut reader, &topic(), &mut sender, &mut throttle)
            .await
            .unwrap();

        assert_eq!(summary.total_count, 0);
        assert!(sender.sent.is_empty());
    }

    #[tokio::test]
    async fn ac_pub_03_stops_at_data_offset_ignoring_trailing_bytes() {
        let mut file = build_binary(&[b"one"]);
        file.extend_from_slice(b"TRAILING-GARBAGE");
        let mut sender = MockSender::default();
        let mut throttle = Throttle::new(0, 0);
        let mut reader = &file[..];

        let summary = stream_binary_packets(&mut reader, &topic(), &mut sender, &mut throttle)
            .await
            .unwrap();

        assert_eq!(summary.total_count, 1);
    }

    // ---- AC-PUB-04: throttling ----

    #[test]
    fn ac_pub_04_throttle_pauses_after_each_full_batch() {
        let mut t = Throttle::new(3, 100);
        let pauses: Vec<_> = (0..7).map(|_| t.record_send()).collect();
        let d = Some(Duration::from_millis(100));
        assert_eq!(pauses, vec![None, None, d, None, None, d, None]);
    }

    #[test]
    fn ac_pub_04_throttle_disabled_by_zero_batch_or_zero_interval() {
        let mut no_batch = Throttle::new(0, 100);
        assert!((0..50).all(|_| no_batch.record_send().is_none()));

        let mut no_interval = Throttle::new(2, 0);
        assert!((0..50).all(|_| no_interval.record_send().is_none()));
    }

    #[tokio::test]
    async fn ac_pub_04_binary_stream_really_waits_between_batches() {
        let file = build_binary(&[b"a", b"b", b"c", b"d"]);
        let mut sender = MockSender::default();
        let mut throttle = Throttle::new(2, 40); // 4 packets -> 2 pauses
        let mut reader = &file[..];

        let start = Instant::now();
        stream_binary_packets(&mut reader, &topic(), &mut sender, &mut throttle)
            .await
            .unwrap();

        assert!(start.elapsed() >= Duration::from_millis(80));
        assert_eq!(sender.sent.len(), 4);
    }

    // ---- AC-PUB-05: --file auto-detection ----

    #[tokio::test]
    async fn ac_pub_05_publish_file_detects_text_file() {
        let path = TempPath::with_bytes("pub_text.txt", b"a\nb\n");
        let mut sender = MockSender::default();
        publish_file(path.as_str(), &topic(), &mut sender, &mut Throttle::new(0, 0))
            .await
            .unwrap();
        assert_eq!(sender.sent.len(), 2);
    }

    #[tokio::test]
    async fn ac_pub_05_publish_file_detects_binary_file() {
        let path = TempPath::with_bytes("pub_bin.dat", &build_binary(&[&[0xff, 0xfe], &[0xfd]]));
        let mut sender = MockSender::default();
        publish_file(path.as_str(), &topic(), &mut sender, &mut Throttle::new(0, 0))
            .await
            .unwrap();
        assert_eq!(sender.sent.len(), 2);
        assert_eq!(sender.sent[0].1, vec![0xff, 0xfe]);
    }

    #[tokio::test]
    async fn ac_pub_05_large_cjk_text_cut_at_probe_boundary_stays_text() {
        // 500 x 3-byte chars on the first line: the 1024-byte probe ends inside a character.
        let content = format!("{}\n好\n", "你".repeat(500));
        assert!(content.as_bytes().len() > PROBE_SIZE);
        let path = TempPath::with_bytes("pub_cjk.txt", content.as_bytes());
        let mut sender = MockSender::default();

        publish_file(path.as_str(), &topic(), &mut sender, &mut Throttle::new(0, 0))
            .await
            .unwrap();

        assert_eq!(sender.sent.len(), 2);
        assert_eq!(sender.sent[0].1, "你".repeat(500).into_bytes());
        assert_eq!(sender.sent[1].1, "好".as_bytes());
    }

    #[tokio::test]
    async fn ac_pub_05_missing_file_is_an_error_not_a_panic() {
        let mut sender = MockSender::default();
        let result = publish_file(
            "definitely/does/not/exist.txt",
            &topic(),
            &mut sender,
            &mut Throttle::new(0, 0),
        )
        .await;
        assert!(result.is_err());
    }

    // ---- AC-PUB-06: topic validation ----

    #[test]
    fn ac_pub_06_validate_pub_topic_rejects_empty_and_asterisk() {
        assert!(validate_pub_topic("").is_err());
        assert!(validate_pub_topic("   ").is_err());
        assert!(validate_pub_topic("*").is_err());
        assert!(validate_pub_topic(" * ").is_err());

        assert_eq!(validate_pub_topic("sensor"), Ok("sensor"));
        assert_eq!(validate_pub_topic("  sensor  "), Ok("sensor"));
        assert_eq!(validate_pub_topic("*sensor"), Ok("*sensor"));
    }
}
