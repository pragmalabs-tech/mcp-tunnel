#![allow(non_snake_case)]
//! End-to-end streaming tests for the mcp-tunnel relay + tunnel-client.
//!
//! Spins up:
//!   - an axum upstream server with `/json` (small response) and `/sse`
//!     (long-lived event-stream)
//!   - the mcp-tunnel relay on a random port
//!   - a tunnel-client connecting the upstream to the relay
//!
//! Then makes reqwest calls against the relay's public URL (via Host
//! header) and asserts streaming behavior.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::response::sse::{Event, Sse};
use axum::routing::get;
use futures_util::StreamExt;
use mcp_tunnel::config::RelayConfig;
use mcp_tunnel_client::{TunnelStatusCallback, start_tunnel_client};
use tokio::sync::Mutex;

const TEST_DOMAIN: &str = "tunnel.test";
const TEST_TOKEN: &str = "tok-streaming";

// ── Test harness ────────────────────────────────────────────────

struct SilentStatus;
impl TunnelStatusCallback for SilentStatus {
    fn on_connected(&self, _url: &str) {}
    fn on_disconnected(&self) {}
    fn on_evicted(&self) {}
}

#[derive(Clone, Default)]
struct UpstreamMetrics {
    /// Active SSE connections to /sse, observed via guard drop.
    active_sse: Arc<AtomicUsize>,
}

struct ActiveGuard(Arc<AtomicUsize>);
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn spawn_upstream() -> (u16, UpstreamMetrics) {
    let metrics = UpstreamMetrics::default();
    let metrics_for_route = metrics.clone();
    let app = Router::new()
        .route(
            "/json",
            get(|| async { axum::Json(serde_json::json!({"ok": true})) }),
        )
        .route(
            "/sse",
            get(move || {
                let counter = metrics_for_route.active_sse.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let guard = Arc::new(Mutex::new(Some(ActiveGuard(counter))));
                    let stream = futures_util::stream::unfold((0u32, guard), |(i, g)| async move {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        let event = Event::default()
                            .event("message")
                            .data(format!("{{\"i\":{i}}}"));
                        Some((Ok::<_, Infallible>(event), (i + 1, g)))
                    });
                    Sse::new(stream)
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (port, metrics)
}

async fn spawn_relay() -> u16 {
    let mut tokens = HashMap::new();
    tokens.insert(TEST_TOKEN.to_string(), vec!["weather".to_string()]);
    let cfg = RelayConfig {
        port: 0,
        relay_domain: TEST_DOMAIN.to_string(),
        auth_provider: None,
        auth_provider_secret: None,
        tokens,
        max_request_body_size: Some(5 * 1024 * 1024),
    };
    let (app, _) = mcp_tunnel::relay::build_relay_app(cfg);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    port
}

async fn start_client(upstream_port: u16, relay_port: u16) {
    let relay_url = format!("ws://127.0.0.1:{relay_port}");
    start_tunnel_client(
        upstream_port,
        &relay_url,
        TEST_TOKEN,
        Some("weather"),
        SilentStatus,
    )
    .await
    .expect("tunnel-client connects");
    // tunnel-client spawns its background loop; give it a moment to register
    tokio::time::sleep(Duration::from_millis(50)).await;
}

fn relay_url(relay_port: u16) -> String {
    format!("http://127.0.0.1:{relay_port}/")
}

fn host_header() -> String {
    format!("weather.{TEST_DOMAIN}")
}

// ── Tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn streaming__small_json_response_round_trips() {
    let (upstream, _) = spawn_upstream().await;
    let relay = spawn_relay().await;
    start_client(upstream, relay).await;

    let resp = reqwest::Client::new()
        .get(format!("{}json", relay_url(relay)))
        .header("host", host_header())
        .send()
        .await
        .expect("relay reachable");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn streaming__sse_events_arrive_promptly_not_buffered() {
    let (upstream, _) = spawn_upstream().await;
    let relay = spawn_relay().await;
    start_client(upstream, relay).await;

    let resp = reqwest::Client::new()
        .get(format!("{}sse", relay_url(relay)))
        .header("host", host_header())
        .header("accept", "text/event-stream")
        .send()
        .await
        .expect("relay reachable");
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .starts_with("text/event-stream"),
        "expected text/event-stream content-type"
    );

    let started = Instant::now();
    let mut stream = resp.bytes_stream();
    let mut received = String::new();
    let deadline = started + Duration::from_secs(2);

    while Instant::now() < deadline && received.matches("data:").count() < 3 {
        match tokio::time::timeout(Duration::from_millis(200), stream.next()).await {
            Ok(Some(Ok(chunk))) => received.push_str(std::str::from_utf8(&chunk).unwrap_or("")),
            _ => break,
        }
    }

    assert!(
        received.matches("data:").count() >= 3,
        "expected at least 3 SSE events, got: {received:?}"
    );
    // The whole window is 2s. Buffered behavior would have us see nothing
    // until the upstream closes (which never happens), so receiving any
    // event proves streaming.
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "received events too slowly, suggesting buffering"
    );
}

#[tokio::test]
async fn streaming__client_disconnect_cancels_upstream_sse() {
    let (upstream, metrics) = spawn_upstream().await;
    let relay = spawn_relay().await;
    start_client(upstream, relay).await;

    let resp = reqwest::Client::new()
        .get(format!("{}sse", relay_url(relay)))
        .header("host", host_header())
        .header("accept", "text/event-stream")
        .send()
        .await
        .expect("relay reachable");

    // Read a couple events then drop the response (simulating client disconnect).
    let mut stream = resp.bytes_stream();
    let _ = tokio::time::timeout(Duration::from_millis(500), stream.next()).await;
    let _ = tokio::time::timeout(Duration::from_millis(500), stream.next()).await;

    assert_eq!(
        metrics.active_sse.load(Ordering::SeqCst),
        1,
        "upstream SSE connection should be active before drop"
    );

    drop(stream);

    // The cancel watcher polls every 50ms + axum tear-down is async; give it a
    // generous window.
    let dropped_within = async {
        for _ in 0..40 {
            if metrics.active_sse.load(Ordering::SeqCst) == 0 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }
    .await;

    assert!(
        dropped_within,
        "upstream SSE connection should drop within 2s of client disconnect"
    );
}
