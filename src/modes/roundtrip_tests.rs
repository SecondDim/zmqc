//! End-to-end acceptance tests: a real `PubSocket` talking to a real `SubSocket` over
//! TCP loopback, using the same helpers the CLI modes use.

use super::pub_mode::{publish_file, Throttle, TwoPartSender};
use super::sub_mode::{format_message_line, parse_message_frames};
use super::test_util::TempPath;
use bytes::Bytes;
use std::time::Duration;
use zeromq::{PubSocket, Socket, SocketRecv, SocketSend, SubSocket, ZmqMessage};

const RECV_TIMEOUT: Duration = Duration::from_secs(5);
/// Time for the SUB handshake + subscription to propagate (the "slow joiner" window).
const HANDSHAKE: Duration = Duration::from_millis(400);

async fn bound_publisher() -> (PubSocket, String) {
    let mut publisher = PubSocket::new();
    let endpoint = publisher.bind("tcp://127.0.0.1:0").await.unwrap();
    (publisher, endpoint.to_string())
}

async fn connected_subscriber(endpoint: &str, topic: &str) -> SubSocket {
    let mut sub = SubSocket::new();
    sub.connect(endpoint).await.unwrap();
    sub.subscribe(topic).await.unwrap();
    tokio::time::sleep(HANDSHAKE).await;
    sub
}

async fn recv(sub: &mut SubSocket) -> ZmqMessage {
    tokio::time::timeout(RECV_TIMEOUT, sub.recv())
        .await
        .expect("timed out waiting for a message")
        .unwrap()
}

// AC-E2E-01: the publisher's [topic, payload] arrives intact and is rendered as `[n] topic => payload`.
#[tokio::test]
async fn ac_e2e_01_two_frame_message_roundtrip() {
    let (mut publisher, endpoint) = bound_publisher().await;
    let mut sub = connected_subscriber(&endpoint, "").await;

    publisher
        .send_two_part(&Bytes::from_static(b"sensor"), Bytes::from_static(b"21.5"))
        .await
        .unwrap();

    let (topic, payload) = parse_message_frames(&recv(&mut sub).await);
    assert_eq!(format_message_line(1, &topic, &payload), "[1] sensor => 21.5");
}

// AC-E2E-02: a topic filter only lets matching topics through.
#[tokio::test]
async fn ac_e2e_02_topic_filter_drops_other_topics() {
    let (mut publisher, endpoint) = bound_publisher().await;
    let mut sub = connected_subscriber(&endpoint, "wanted").await;

    publisher
        .send_two_part(&Bytes::from_static(b"other"), Bytes::from_static(b"nope"))
        .await
        .unwrap();
    publisher
        .send_two_part(&Bytes::from_static(b"wanted"), Bytes::from_static(b"yes"))
        .await
        .unwrap();

    let (topic, payload) = parse_message_frames(&recv(&mut sub).await);
    assert_eq!((topic.as_str(), payload.as_str()), ("wanted", "yes"));
}

// AC-E2E-03: an empty subscription (no --topic) receives every topic.
#[tokio::test]
async fn ac_e2e_03_empty_subscription_receives_all_topics() {
    let (mut publisher, endpoint) = bound_publisher().await;
    let mut sub = connected_subscriber(&endpoint, "").await;

    for t in ["a", "b"] {
        publisher
            .send_two_part(&Bytes::copy_from_slice(t.as_bytes()), Bytes::from_static(b"x"))
            .await
            .unwrap();
    }

    let first = parse_message_frames(&recv(&mut sub).await).0;
    let second = parse_message_frames(&recv(&mut sub).await).0;
    assert_eq!((first.as_str(), second.as_str()), ("a", "b"));
}

// AC-E2E-04: non-UTF-8 payloads survive the wire and are shown as hex.
#[tokio::test]
async fn ac_e2e_04_binary_payload_is_displayed_as_hex() {
    let (mut publisher, endpoint) = bound_publisher().await;
    let mut sub = connected_subscriber(&endpoint, "").await;

    publisher
        .send_two_part(
            &Bytes::from_static(b"bin"),
            Bytes::from_static(&[0xff, 0x00, 0x10]),
        )
        .await
        .unwrap();

    let (topic, payload) = parse_message_frames(&recv(&mut sub).await);
    assert_eq!((topic.as_str(), payload.as_str()), ("bin", "ff0010"));
}

// AC-E2E-05: a foreign single-frame message is shown with an empty topic.
#[tokio::test]
async fn ac_e2e_05_single_frame_message_from_foreign_publisher() {
    let (mut publisher, endpoint) = bound_publisher().await;
    let mut sub = connected_subscriber(&endpoint, "").await;

    publisher.send(ZmqMessage::from("lonely")).await.unwrap();

    let (topic, payload) = parse_message_frames(&recv(&mut sub).await);
    assert_eq!((topic.as_str(), payload.as_str()), ("", "lonely"));
}

// AC-E2E-06: `--file` text content is published line by line, empty lines skipped.
#[tokio::test]
async fn ac_e2e_06_text_file_is_published_line_by_line() {
    let file = TempPath::with_bytes("e2e_text.txt", b"one\n\ntwo\nthree\n");
    let (mut publisher, endpoint) = bound_publisher().await;
    let mut sub = connected_subscriber(&endpoint, "file").await;

    publish_file(
        file.as_str(),
        &Bytes::from_static(b"file"),
        &mut publisher,
        &mut Throttle::new(0, 0),
    )
    .await
    .unwrap();

    let mut got = Vec::new();
    for _ in 0..3 {
        got.push(parse_message_frames(&recv(&mut sub).await).1);
    }
    assert_eq!(got, vec!["one", "two", "three"]);
}

// AC-E2E-07: a connecting publisher (no --bind) works against a binding subscriber.
#[tokio::test]
async fn ac_e2e_07_subscriber_can_bind_and_publisher_connect() {
    let mut sub = SubSocket::new();
    let endpoint = sub.bind("tcp://127.0.0.1:0").await.unwrap().to_string();
    sub.subscribe("").await.unwrap();

    let mut publisher = PubSocket::new();
    publisher.connect(&endpoint).await.unwrap();
    tokio::time::sleep(HANDSHAKE).await;

    publisher
        .send_two_part(&Bytes::from_static(b"t"), Bytes::from_static(b"reverse"))
        .await
        .unwrap();

    let (topic, payload) = parse_message_frames(&recv(&mut sub).await);
    assert_eq!((topic.as_str(), payload.as_str()), ("t", "reverse"));
}
// AC-E2E-08: connecting two incompatible sockets (e.g. PUB to PUB or SUB to SUB)
// fails with the ZeroMQ protocol incompatibility error.
#[tokio::test]
async fn ac_e2e_08_incompatible_pub_to_pub_connection_fails() {
    let mut p1 = PubSocket::new();
    let endpoint = p1.bind("tcp://127.0.0.1:0").await.unwrap().to_string();

    let mut p2 = PubSocket::new();
    let err = p2.connect(&endpoint).await.unwrap_err();
    assert!(
        err.to_string().contains("Provided sockets combination is not compatible"),
        "Expected socket incompatibility error, got: {err}"
    );
}

#[tokio::test]
async fn ac_e2e_08_incompatible_sub_to_sub_connection_fails() {
    let mut s1 = SubSocket::new();
    let endpoint = s1.bind("tcp://127.0.0.1:0").await.unwrap().to_string();

    let mut s2 = SubSocket::new();
    let err = s2.connect(&endpoint).await.unwrap_err();
    assert!(
        err.to_string().contains("Provided sockets combination is not compatible"),
        "Expected socket incompatibility error, got: {err}"
    );
}

// AC-E2E-09: wildcard topic in SUB mode (starting with '*') receives messages across any topic.
#[tokio::test]
async fn ac_e2e_09_wildcard_topic_receives_all_messages() {
    use crate::modes::sub_mode::resolve_sub_topic;

    let (mut publisher, endpoint) = bound_publisher().await;
    let resolved = resolve_sub_topic("*");
    assert_eq!(resolved, "");

    let mut sub = connected_subscriber(&endpoint, resolved).await;

    for topic in ["alpha", "beta", "gamma"] {
        publisher
            .send_two_part(&Bytes::copy_from_slice(topic.as_bytes()), Bytes::from_static(b"data"))
            .await
            .unwrap();
    }

    let mut received_topics = Vec::new();
    for _ in 0..3 {
        let (topic, _) = parse_message_frames(&recv(&mut sub).await);
        received_topics.push(topic);
    }
    assert_eq!(received_topics, vec!["alpha", "beta", "gamma"]);
}

