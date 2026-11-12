use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use super::broker_mode::BrokerStats;
use super::console::ConnCounter;
use crate::Mode;

/// Prometheus text exposition format 0.0.4 Content-Type header value.
pub(crate) const METRICS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Plain text Content-Type for root / error responses.
pub(crate) const TEXT_PLAIN_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

/// Maximum HTTP request header size accepted by the metrics server (8 KiB).
const MAX_REQUEST_BYTES: usize = 8 * 1024;

/// Per-connection read/write timeout (5 seconds).
const CONN_TIMEOUT: Duration = Duration::from_secs(5);

/// Point-in-time snapshot of metrics values used by the pure Prometheus renderer.
#[derive(Debug, Clone)]
pub(crate) struct MetricsSnapshot {
    pub(crate) mode: Mode,
    pub(crate) version: &'static str,
    pub(crate) uptime_secs: u64,
    pub(crate) conns: Vec<(&'static str, usize)>,
    // Proxy counters
    pub(crate) proxy_forwarded: u64,
    pub(crate) proxy_sub_events: u64,
    // Broker counters & gauges
    pub(crate) broker_published: u64,
    pub(crate) broker_commands: u64,
    pub(crate) broker_registered: u64,
    pub(crate) broker_unregistered: u64,
    pub(crate) broker_dropped: u64,
    pub(crate) broker_workers: u64,
}

/// Shared lock-free metrics state updated by the proxy/broker loop and read by the HTTP task.
pub(crate) struct MetricsState {
    started: Instant,
    mode: Mode,
    conns: Vec<(&'static str, ConnCounter)>,
    // Proxy counters
    proxy_forwarded: AtomicU64,
    proxy_sub_events: AtomicU64,
    // Broker counters & gauges
    broker_published: AtomicU64,
    broker_commands: AtomicU64,
    broker_registered: AtomicU64,
    broker_unregistered: AtomicU64,
    broker_dropped: AtomicU64,
    broker_workers: AtomicU64,
}

impl MetricsState {
    /// Creates a new shared `MetricsState` for `Mode::Proxy` or `Mode::Broker`.
    pub(crate) fn new(mode: Mode, conns: Vec<(&'static str, ConnCounter)>) -> Arc<Self> {
        Arc::new(Self {
            started: Instant::now(),
            mode,
            conns,
            proxy_forwarded: AtomicU64::new(0),
            proxy_sub_events: AtomicU64::new(0),
            broker_published: AtomicU64::new(0),
            broker_commands: AtomicU64::new(0),
            broker_registered: AtomicU64::new(0),
            broker_unregistered: AtomicU64::new(0),
            broker_dropped: AtomicU64::new(0),
            broker_workers: AtomicU64::new(0),
        })
    }

    /// Mirrors proxy mode counters into atomic storage.
    pub(crate) fn set_proxy(&self, forwarded: usize, sub_events: usize) {
        self.proxy_forwarded
            .store(forwarded as u64, Ordering::Relaxed);
        self.proxy_sub_events
            .store(sub_events as u64, Ordering::Relaxed);
    }

    /// Mirrors broker mode counters and registered worker gauge into atomic storage.
    pub(crate) fn sync_broker(&self, stats: &BrokerStats, workers: usize) {
        self.broker_published
            .store(stats.published as u64, Ordering::Relaxed);
        self.broker_commands
            .store(stats.commands as u64, Ordering::Relaxed);
        self.broker_registered
            .store(stats.registered as u64, Ordering::Relaxed);
        self.broker_unregistered
            .store(stats.unregistered as u64, Ordering::Relaxed);
        self.broker_dropped
            .store(stats.dropped as u64, Ordering::Relaxed);
        self.broker_workers
            .store(workers as u64, Ordering::Relaxed);
    }

    /// Captures a point-in-time snapshot of all metrics.
    pub(crate) fn snapshot(&self) -> MetricsSnapshot {
        let conns = self
            .conns
            .iter()
            .map(|(name, c)| (*name, c.load(Ordering::Relaxed)))
            .collect();
        MetricsSnapshot {
            mode: self.mode,
            version: env!("CARGO_PKG_VERSION"),
            uptime_secs: self.started.elapsed().as_secs(),
            conns,
            proxy_forwarded: self.proxy_forwarded.load(Ordering::Relaxed),
            proxy_sub_events: self.proxy_sub_events.load(Ordering::Relaxed),
            broker_published: self.broker_published.load(Ordering::Relaxed),
            broker_commands: self.broker_commands.load(Ordering::Relaxed),
            broker_registered: self.broker_registered.load(Ordering::Relaxed),
            broker_unregistered: self.broker_unregistered.load(Ordering::Relaxed),
            broker_dropped: self.broker_dropped.load(Ordering::Relaxed),
            broker_workers: self.broker_workers.load(Ordering::Relaxed),
        }
    }

    /// Renders the current state in Prometheus text format 0.0.4.
    pub(crate) fn render(&self) -> String {
        render_prometheus(&self.snapshot())
    }
}

/// Escapes a Prometheus label value (`\`, `"`, and `\n` per Prometheus text format 0.0.4).
pub(crate) fn escape_label_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for ch in v.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// Renders a `MetricsSnapshot` into Prometheus text exposition format 0.0.4.
pub(crate) fn render_prometheus(s: &MetricsSnapshot) -> String {
    let mut out = String::with_capacity(1024);
    let mode_label = match s.mode {
        Mode::Proxy => "proxy",
        Mode::Broker => "broker",
        Mode::Pub => "pub",
        Mode::Sub => "sub",
        Mode::Dealer => "dealer",
        Mode::Router => "router",
    };

    let _ = writeln!(out, "# HELP zmqc_up Whether zmqc is running (always 1).");
    let _ = writeln!(out, "# TYPE zmqc_up gauge");
    let _ = writeln!(out, "zmqc_up 1");

    let _ = writeln!(
        out,
        "# HELP zmqc_uptime_seconds Elapsed uptime in seconds since zmqc started."
    );
    let _ = writeln!(out, "# TYPE zmqc_uptime_seconds gauge");
    let _ = writeln!(out, "zmqc_uptime_seconds {}", s.uptime_secs);

    let _ = writeln!(out, "# HELP zmqc_build_info Build and mode information.");
    let _ = writeln!(out, "# TYPE zmqc_build_info gauge");
    let _ = writeln!(
        out,
        "zmqc_build_info{{version=\"{}\",mode=\"{}\"}} 1",
        escape_label_value(s.version),
        escape_label_value(mode_label)
    );

    let _ = writeln!(
        out,
        "# HELP zmqc_connections Current peer connections per socket (requires --monitor)."
    );
    let _ = writeln!(out, "# TYPE zmqc_connections gauge");
    for (sock_name, count) in &s.conns {
        let _ = writeln!(
            out,
            "zmqc_connections{{socket=\"{}\"}} {}",
            escape_label_value(sock_name),
            count
        );
    }

    match s.mode {
        Mode::Proxy => {
            let _ = writeln!(
                out,
                "# HELP zmqc_proxy_messages_forwarded_total Total data messages forwarded from XSUB to XPUB."
            );
            let _ = writeln!(out, "# TYPE zmqc_proxy_messages_forwarded_total counter");
            let _ = writeln!(
                out,
                "zmqc_proxy_messages_forwarded_total {}",
                s.proxy_forwarded
            );

            let _ = writeln!(
                out,
                "# HELP zmqc_proxy_subscription_events_total Total subscription/unsubscription events forwarded from XPUB to XSUB."
            );
            let _ = writeln!(out, "# TYPE zmqc_proxy_subscription_events_total counter");
            let _ = writeln!(
                out,
                "zmqc_proxy_subscription_events_total {}",
                s.proxy_sub_events
            );
        }
        Mode::Broker => {
            let _ = writeln!(
                out,
                "# HELP zmqc_broker_messages_published_total Total business messages published via PUB socket."
            );
            let _ = writeln!(out, "# TYPE zmqc_broker_messages_published_total counter");
            let _ = writeln!(
                out,
                "zmqc_broker_messages_published_total {}",
                s.broker_published
            );

            let _ = writeln!(
                out,
                "# HELP zmqc_broker_commands_broadcast_total Total control commands broadcast to registered workers."
            );
            let _ = writeln!(out, "# TYPE zmqc_broker_commands_broadcast_total counter");
            let _ = writeln!(
                out,
                "zmqc_broker_commands_broadcast_total {}",
                s.broker_commands
            );

            let _ = writeln!(
                out,
                "# HELP zmqc_broker_registrations_total Total worker _R registration messages processed."
            );
            let _ = writeln!(out, "# TYPE zmqc_broker_registrations_total counter");
            let _ = writeln!(out, "zmqc_broker_registrations_total {}", s.broker_registered);

            let _ = writeln!(
                out,
                "# HELP zmqc_broker_unregistrations_total Total worker _U unregistration messages processed."
            );
            let _ = writeln!(out, "# TYPE zmqc_broker_unregistrations_total counter");
            let _ = writeln!(
                out,
                "zmqc_broker_unregistrations_total {}",
                s.broker_unregistered
            );

            let _ = writeln!(
                out,
                "# HELP zmqc_broker_messages_dropped_total Total malformed router messages dropped."
            );
            let _ = writeln!(out, "# TYPE zmqc_broker_messages_dropped_total counter");
            let _ = writeln!(out, "zmqc_broker_messages_dropped_total {}", s.broker_dropped);

            let _ = writeln!(
                out,
                "# HELP zmqc_broker_registered_workers Current number of registered workers in WorkerRegistry."
            );
            let _ = writeln!(out, "# TYPE zmqc_broker_registered_workers gauge");
            let _ = writeln!(out, "zmqc_broker_registered_workers {}", s.broker_workers);
        }
        _ => {}
    }

    out
}

/// Structured HTTP response produced by `route_request`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HttpResponse {
    pub(crate) status_line: &'static str,
    pub(crate) content_type: &'static str,
    pub(crate) extra_headers: &'static str,
    pub(crate) body: String,
}

impl HttpResponse {
    /// Serializes this HTTP response into raw wire bytes.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let header = format!(
            "{}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n",
            self.status_line,
            self.content_type,
            self.body.len(),
            self.extra_headers,
        );
        let mut bytes = Vec::with_capacity(header.len() + self.body.len());
        bytes.extend_from_slice(header.as_bytes());
        bytes.extend_from_slice(self.body.as_bytes());
        bytes
    }
}

/// Routes a single HTTP request line (`METHOD SP TARGET SP VERSION`) to an `HttpResponse`.
pub(crate) fn route_request(request_line: &str, state: &MetricsState) -> HttpResponse {
    let mut parts = request_line.trim().split_whitespace();
    let (Some(method), Some(target), Some(version)) =
        (parts.next(), parts.next(), parts.next())
    else {
        return HttpResponse {
            status_line: "HTTP/1.1 400 Bad Request",
            content_type: TEXT_PLAIN_CONTENT_TYPE,
            extra_headers: "",
            body: "400 Bad Request\n".to_string(),
        };
    };

    if parts.next().is_some() || !version.starts_with("HTTP/1.") {
        return HttpResponse {
            status_line: "HTTP/1.1 400 Bad Request",
            content_type: TEXT_PLAIN_CONTENT_TYPE,
            extra_headers: "",
            body: "400 Bad Request\n".to_string(),
        };
    }

    if method != "GET" {
        return HttpResponse {
            status_line: "HTTP/1.1 405 Method Not Allowed",
            content_type: TEXT_PLAIN_CONTENT_TYPE,
            extra_headers: "Allow: GET\r\n",
            body: "405 Method Not Allowed\n".to_string(),
        };
    }

    let path = target.split('?').next().unwrap_or(target);
    match path {
        "/metrics" => HttpResponse {
            status_line: "HTTP/1.1 200 OK",
            content_type: METRICS_CONTENT_TYPE,
            extra_headers: "",
            body: state.render(),
        },
        "/" => HttpResponse {
            status_line: "HTTP/1.1 200 OK",
            content_type: TEXT_PLAIN_CONTENT_TYPE,
            extra_headers: "",
            body: "zmqc metrics exporter — see /metrics\n".to_string(),
        },
        _ => HttpResponse {
            status_line: "HTTP/1.1 404 Not Found",
            content_type: TEXT_PLAIN_CONTENT_TYPE,
            extra_headers: "",
            body: "404 Not Found\n".to_string(),
        },
    }
}

/// Handles a single incoming TCP connection (reads up to `\r\n\r\n` or `MAX_REQUEST_BYTES`, writes response).
async fn handle_connection(mut stream: TcpStream, state: &MetricsState) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 512];

    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.windows(2).any(|w| w == b"\n\n") {
            break;
        }
        if buf.len() >= MAX_REQUEST_BYTES {
            return Ok(());
        }
    }

    if buf.is_empty() {
        return Ok(());
    }

    let raw = String::from_utf8_lossy(&buf);
    let request_line = raw.lines().next().unwrap_or("");
    let resp = route_request(request_line, state);
    stream.write_all(&resp.to_bytes()).await?;
    stream.flush().await?;
    let _ = stream.shutdown().await;
    Ok(())
}

/// Binds the metrics HTTP listener on `addr` and spawns the single-connection sequential accept loop.
/// Returns the bound `SocketAddr` (useful when binding to port 0 in tests) and task `JoinHandle`.
pub(crate) async fn spawn_metrics_server(
    addr: SocketAddr,
    state: Arc<MetricsState>,
) -> std::io::Result<(SocketAddr, JoinHandle<()>)> {
    let listener = TcpListener::bind(addr).await?;
    let local_addr = listener.local_addr()?;

    let handle = tokio::spawn(async move {
        // Sequential single-connection loop: processes at most 1 connection at a time.
        while let Ok((stream, _peer)) = listener.accept().await {
            let _ = tokio::time::timeout(CONN_TIMEOUT, handle_connection(stream, &state)).await;
        }
    });

    Ok((local_addr, handle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn sample_proxy_snapshot() -> MetricsSnapshot {
        MetricsSnapshot {
            mode: Mode::Proxy,
            version: "0.1.0",
            uptime_secs: 42,
            conns: vec![("xpub", 3), ("xsub", 1)],
            proxy_forwarded: 150,
            proxy_sub_events: 7,
            broker_published: 0,
            broker_commands: 0,
            broker_registered: 0,
            broker_unregistered: 0,
            broker_dropped: 0,
            broker_workers: 0,
        }
    }

    fn sample_broker_snapshot() -> MetricsSnapshot {
        MetricsSnapshot {
            mode: Mode::Broker,
            version: "0.1.0",
            uptime_secs: 120,
            conns: vec![("router", 4), ("pub", 2)],
            proxy_forwarded: 0,
            proxy_sub_events: 0,
            broker_published: 500,
            broker_commands: 12,
            broker_registered: 5,
            broker_unregistered: 2,
            broker_dropped: 3,
            broker_workers: 3,
        }
    }

    // AC-MET-01: render_prometheus in Proxy mode outputs proxy metrics & socket labels, and omits broker metrics
    #[test]
    fn ac_met_01_render_prometheus_proxy() {
        let text = render_prometheus(&sample_proxy_snapshot());

        assert!(text.contains("# TYPE zmqc_up gauge\nzmqc_up 1\n"));
        assert!(text.contains("# TYPE zmqc_uptime_seconds gauge\nzmqc_uptime_seconds 42\n"));
        assert!(text.contains("zmqc_build_info{version=\"0.1.0\",mode=\"proxy\"} 1\n"));
        assert!(text.contains("zmqc_connections{socket=\"xpub\"} 3\n"));
        assert!(text.contains("zmqc_connections{socket=\"xsub\"} 1\n"));
        assert!(text.contains(
            "# TYPE zmqc_proxy_messages_forwarded_total counter\nzmqc_proxy_messages_forwarded_total 150\n"
        ));
        assert!(text.contains(
            "# TYPE zmqc_proxy_subscription_events_total counter\nzmqc_proxy_subscription_events_total 7\n"
        ));

        // Must not contain broker-specific metrics
        assert!(!text.contains("zmqc_broker_"));
    }

    // AC-MET-02: render_prometheus in Broker mode outputs broker metrics & socket labels, and omits proxy metrics
    #[test]
    fn ac_met_02_render_prometheus_broker() {
        let text = render_prometheus(&sample_broker_snapshot());

        assert!(text.contains("# TYPE zmqc_up gauge\nzmqc_up 1\n"));
        assert!(text.contains("# TYPE zmqc_uptime_seconds gauge\nzmqc_uptime_seconds 120\n"));
        assert!(text.contains("zmqc_build_info{version=\"0.1.0\",mode=\"broker\"} 1\n"));
        assert!(text.contains("zmqc_connections{socket=\"router\"} 4\n"));
        assert!(text.contains("zmqc_connections{socket=\"pub\"} 2\n"));
        assert!(text.contains("zmqc_broker_messages_published_total 500\n"));
        assert!(text.contains("zmqc_broker_commands_broadcast_total 12\n"));
        assert!(text.contains("zmqc_broker_registrations_total 5\n"));
        assert!(text.contains("zmqc_broker_unregistrations_total 2\n"));
        assert!(text.contains("zmqc_broker_messages_dropped_total 3\n"));
        assert!(text.contains(
            "# TYPE zmqc_broker_registered_workers gauge\nzmqc_broker_registered_workers 3\n"
        ));

        // Must not contain proxy-specific metrics
        assert!(!text.contains("zmqc_proxy_"));
    }

    // AC-MET-03: route_request handles GET /metrics, GET /, 404, 405, and 400
    #[test]
    fn ac_met_03_route_request() {
        let state = MetricsState::new(
            Mode::Proxy,
            vec![
                ("xpub", Arc::new(AtomicUsize::new(2))),
                ("xsub", Arc::new(AtomicUsize::new(1))),
            ],
        );
        state.set_proxy(9, 4);

        // GET /metrics
        let r_metrics = route_request("GET /metrics HTTP/1.1", &state);
        assert_eq!(r_metrics.status_line, "HTTP/1.1 200 OK");
        assert_eq!(r_metrics.content_type, METRICS_CONTENT_TYPE);
        assert!(r_metrics.body.contains("zmqc_proxy_messages_forwarded_total 9\n"));

        // GET /metrics with query string
        let r_query = route_request("GET /metrics?debug=1 HTTP/1.1", &state);
        assert_eq!(r_query.status_line, "HTTP/1.1 200 OK");

        // GET /
        let r_root = route_request("GET / HTTP/1.1", &state);
        assert_eq!(r_root.status_line, "HTTP/1.1 200 OK");
        assert!(r_root.body.contains("/metrics"));

        // Unknown path => 404
        let r_404 = route_request("GET /nope HTTP/1.1", &state);
        assert_eq!(r_404.status_line, "HTTP/1.1 404 Not Found");

        // Non-GET methods => 405 with Allow: GET
        for m in ["POST /metrics HTTP/1.1", "HEAD /metrics HTTP/1.1", "DELETE / HTTP/1.1"] {
            let r_405 = route_request(m, &state);
            assert_eq!(r_405.status_line, "HTTP/1.1 405 Method Not Allowed", "{m}");
            assert_eq!(r_405.extra_headers, "Allow: GET\r\n", "{m}");
        }

        // Malformed request lines => 400
        for bad in ["", "GET", "GET /metrics", "GET /metrics FTP/1.0", "GET / a b HTTP/1.1"] {
            let r_400 = route_request(bad, &state);
            assert_eq!(r_400.status_line, "HTTP/1.1 400 Bad Request", "{bad}");
        }
    }

    // AC-MET-04: MetricsState syncs counters/gauges accurately and escape_label_value escapes special chars
    #[test]
    fn ac_met_04_state_sync_and_label_escape() {
        let r_conn = Arc::new(AtomicUsize::new(2));
        let p_conn = Arc::new(AtomicUsize::new(5));
        let state = MetricsState::new(
            Mode::Broker,
            vec![("router", r_conn.clone()), ("pub", p_conn.clone())],
        );

        let stats1 = BrokerStats {
            published: 10,
            commands: 3,
            registered: 2,
            unregistered: 0,
            dropped: 1,
        };
        state.sync_broker(&stats1, 2);
        let snap1 = state.snapshot();
        assert_eq!(snap1.broker_published, 10);
        assert_eq!(snap1.broker_workers, 2);
        assert_eq!(snap1.conns, vec![("router", 2), ("pub", 5)]);

        // Worker unregisters => gauge decreases
        let stats2 = BrokerStats {
            published: 11,
            commands: 3,
            registered: 2,
            unregistered: 1,
            dropped: 1,
        };
        state.sync_broker(&stats2, 1);
        assert_eq!(state.snapshot().broker_workers, 1);

        assert_eq!(escape_label_value("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }
}

