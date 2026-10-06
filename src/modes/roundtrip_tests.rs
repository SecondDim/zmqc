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

// AC-E2E-10: Dealer <-> Router bidirectional messaging
#[tokio::test]
async fn ac_e2e_10_dealer_router_bidirectional() {
    use super::dealer_mode::{build_dealer_message, parse_dealer_frames, resolve_dealer_topic};
    use super::router_mode::{build_router_message, parse_router_frames};
    use zeromq::{DealerSocket, RouterSocket};

    let mut router = RouterSocket::new();
    let endpoint = router.bind("tcp://127.0.0.1:0").await.unwrap().to_string();

    let mut dealer = DealerSocket::new();
    dealer.connect(&endpoint).await.unwrap();
    tokio::time::sleep(HANDSHAKE).await;

    // Dealer sends to Router
    let ping = build_dealer_message(&resolve_dealer_topic("chat"), Bytes::from_static(b"ping"));
    dealer.send(ping).await.unwrap();

    let r_msg = tokio::time::timeout(RECV_TIMEOUT, router.recv()).await.unwrap().unwrap();
    let (raw_id, _, topic, payload) = parse_router_frames(&r_msg);
    assert_eq!(topic, "chat");
    assert_eq!(payload, "ping");

    // Router replies back to Dealer
    let pong = build_router_message(raw_id, &resolve_dealer_topic("chat"), Bytes::from_static(b"pong"));
    router.send(pong).await.unwrap();

    let d_msg = tokio::time::timeout(RECV_TIMEOUT, dealer.recv()).await.unwrap().unwrap();
    let (d_topic, d_payload) = parse_dealer_frames(&d_msg);
    assert_eq!(d_topic, "chat");
    assert_eq!(d_payload, "pong");
}

// AC-E2E-11: Dealer with custom identity displays as text on Router
#[tokio::test]
async fn ac_e2e_11_dealer_identity_displays_as_text_on_router() {
    use super::dealer_mode::{build_dealer_message, resolve_dealer_topic};
    use super::router_mode::parse_router_frames;
    use zeromq::{DealerSocket, RouterSocket, SocketOptions};

    let mut router = RouterSocket::new();
    let endpoint = router.bind("tcp://127.0.0.1:0").await.unwrap().to_string();

    let mut opts = SocketOptions::default();
    opts.peer_identity(Bytes::from_static(b"alice").try_into().unwrap());
    let mut dealer = DealerSocket::with_options(opts);
    dealer.connect(&endpoint).await.unwrap();
    tokio::time::sleep(HANDSHAKE).await;

    dealer.send(build_dealer_message(&resolve_dealer_topic("*"), Bytes::from_static(b"hi"))).await.unwrap();

    let r_msg = tokio::time::timeout(RECV_TIMEOUT, router.recv()).await.unwrap().unwrap();
    let (raw_id, id_disp, _, payload) = parse_router_frames(&r_msg);
    assert_eq!(raw_id.as_ref(), b"alice");
    assert_eq!(id_disp, "alice");
    assert_eq!(payload, "hi");
}

// AC-E2E-12: Router directs message to specific Dealer using <msg>|<id>
#[tokio::test]
async fn ac_e2e_12_router_directs_message_to_specific_dealer() {
    use super::dealer_mode::{build_dealer_message, parse_dealer_frames, resolve_dealer_topic};
    use super::router_mode::{build_router_message, parse_router_frames, parse_router_input, PeerRegistry};
    use zeromq::{DealerSocket, RouterSocket, SocketOptions};

    let mut router = RouterSocket::new();
    let endpoint = router.bind("tcp://127.0.0.1:0").await.unwrap().to_string();

    let mut opts_a = SocketOptions::default();
    opts_a.peer_identity(Bytes::from_static(b"alice").try_into().unwrap());
    let mut dealer_a = DealerSocket::with_options(opts_a);
    dealer_a.connect(&endpoint).await.unwrap();

    let mut opts_b = SocketOptions::default();
    opts_b.peer_identity(Bytes::from_static(b"bob").try_into().unwrap());
    let mut dealer_b = DealerSocket::with_options(opts_b);
    dealer_b.connect(&endpoint).await.unwrap();
    tokio::time::sleep(HANDSHAKE).await;

    // Both send a ping so router records their identities
    let mut registry = PeerRegistry::default();
    dealer_a.send(build_dealer_message(&resolve_dealer_topic("*"), Bytes::from_static(b"ping_a"))).await.unwrap();
    let msg_a = tokio::time::timeout(RECV_TIMEOUT, router.recv()).await.unwrap().unwrap();
    let (id_a, _, _, _) = parse_router_frames(&msg_a);
    registry.record_peer(id_a);

    dealer_b.send(build_dealer_message(&resolve_dealer_topic("*"), Bytes::from_static(b"ping_b"))).await.unwrap();
    let msg_b = tokio::time::timeout(RECV_TIMEOUT, router.recv()).await.unwrap().unwrap();
    let (id_b, _, _, _) = parse_router_frames(&msg_b);
    registry.record_peer(id_b);

    // Now Router sends targeted message: "private message|alice"
    let (msg_text, target) = parse_router_input("private message|alice");
    let target_id = registry.find_peer(&target.unwrap()).unwrap();
    let router_msg = build_router_message(target_id, &resolve_dealer_topic("*"), Bytes::from(msg_text));
    router.send(router_msg).await.unwrap();

    // Alice should receive it
    let msg_for_alice = tokio::time::timeout(RECV_TIMEOUT, dealer_a.recv()).await.unwrap().unwrap();
    let (_, alice_payload) = parse_dealer_frames(&msg_for_alice);
    assert_eq!(alice_payload, "private message");

    // Bob should NOT receive it (timeout)
    let bob_result = tokio::time::timeout(Duration::from_millis(200), dealer_b.recv()).await;
    assert!(bob_result.is_err(), "Bob should not have received a message directed to Alice");
}

// AC-E2E-13: Router --ack automatically responds with ACK to sender
#[tokio::test]
async fn ac_e2e_13_router_ack_flow() {
    use super::dealer_mode::{build_dealer_message, format_dealer_line, parse_dealer_frames, resolve_dealer_topic};
    use super::router_mode::{build_ack_message, parse_router_frames};
    use zeromq::{DealerSocket, RouterSocket};

    let mut router = RouterSocket::new();
    let endpoint = router.bind("tcp://127.0.0.1:0").await.unwrap().to_string();

    let mut dealer = DealerSocket::new();
    dealer.connect(&endpoint).await.unwrap();
    tokio::time::sleep(HANDSHAKE).await;

    dealer.send(build_dealer_message(&resolve_dealer_topic("*"), Bytes::from_static(b"data"))).await.unwrap();

    let r_msg = tokio::time::timeout(RECV_TIMEOUT, router.recv()).await.unwrap().unwrap();
    let (raw_id, _, _, _) = parse_router_frames(&r_msg);

    // Simulate router --ack auto reply
    let ack = build_ack_message(raw_id);
    router.send(ack).await.unwrap();

    let d_msg = tokio::time::timeout(RECV_TIMEOUT, dealer.recv()).await.unwrap().unwrap();
    let (topic, payload) = parse_dealer_frames(&d_msg);
    assert_eq!(topic, "");
    assert_eq!(payload, "ACK");
    assert_eq!(format_dealer_line(1, &topic, &payload), "[1]  => ACK");
}

// AC-E2E-14: Router sending to unknown identity silently drops without panic
#[tokio::test]
async fn ac_e2e_14_router_send_to_unknown_identity_drops_silently() {
    use super::dealer_mode::resolve_dealer_topic;
    use super::router_mode::build_router_message;
    use zeromq::RouterSocket;

    let mut router = RouterSocket::new();
    let _endpoint = router.bind("tcp://127.0.0.1:0").await.unwrap().to_string();

    let fake_id = Bytes::from_static(b"non-existent");
    let msg = build_router_message(fake_id, &resolve_dealer_topic("*"), Bytes::from_static(b"drop me"));
    // Simulating Router sending behavior: silently drop errors
    let _ = router.send(msg).await;
}

