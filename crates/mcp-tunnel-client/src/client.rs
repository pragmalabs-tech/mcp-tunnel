//! Tunnel client - connects to a relay server and proxies requests to localhost.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use bytes::Bytes;
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use tokio_util::sync::CancellationToken;

use crate::protocol::{
    RegisterAck, RegisterRequest, SubdomainOffer, SubdomainPick, TunnelMessage, TunnelRequest,
    is_hop_by_hop,
};

/// Headers-only timeout. The body can stream as long as the upstream
/// keeps it open. This bound only catches connect/handshake stalls.
const HEADERS_TIMEOUT: Duration = Duration::from_secs(30);

/// Bytes per response chunk frame. Larger = fewer frames but bigger
/// memory bursts; smaller = chattier WS but smoother backpressure.
/// 64 KiB is a sweet spot for SSE traffic.
const CHUNK_SIZE: usize = 64 * 1024;

/// Callback for tunnel status changes.
pub trait TunnelStatusCallback: Send + Sync + 'static {
    fn on_connected(&self, url: &str);
    fn on_disconnected(&self);
    fn on_evicted(&self);
}

/// Connect to a relay server and return the assigned public URL.
/// Spawns a background task that proxies requests from relay to localhost.
/// If `subdomain` is provided, requests that specific subdomain from the relay.
pub async fn start_tunnel_client(
    port: u16,
    relay_url: &str,
    token: &str,
    subdomain: Option<&str>,
    status: impl TunnelStatusCallback,
) -> Result<String, String> {
    let relay = relay_url.trim_end_matches('/');
    let ws_url = if relay.starts_with("ws://") || relay.starts_with("wss://") {
        format!("{relay}/_tunnel/register")
    } else if let Some(rest) = relay.strip_prefix("https://") {
        format!("wss://{rest}/_tunnel/register")
    } else if let Some(rest) = relay.strip_prefix("http://") {
        format!("ws://{rest}/_tunnel/register")
    } else {
        format!("wss://{relay}/_tunnel/register")
    };

    let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .map_err(|e| format!("Failed to connect to relay: {e}"))?;

    let (mut ws_sink, mut ws_stream) = ws_stream.split();

    let reg = RegisterRequest {
        token: token.to_string(),
        subdomain: subdomain.map(|s| s.to_string()),
    };
    ws_sink
        .send(tokio_tungstenite::tungstenite::Message::Text(
            serde_json::to_string(&reg).unwrap().into(),
        ))
        .await
        .map_err(|e| format!("Failed to send registration: {e}"))?;

    let ack: RegisterAck = loop {
        match ws_stream.next().await {
            Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                if let Ok(ack) = serde_json::from_str::<RegisterAck>(&text) {
                    break ack;
                }
                if let Ok(offer) = serde_json::from_str::<SubdomainOffer>(&text) {
                    let picked = pick_subdomain(&offer.subdomains)?;
                    let pick = SubdomainPick { subdomain: picked };
                    ws_sink
                        .send(tokio_tungstenite::tungstenite::Message::Text(
                            serde_json::to_string(&pick).unwrap().into(),
                        ))
                        .await
                        .map_err(|e| format!("Failed to send subdomain pick: {e}"))?;
                    continue;
                }
                continue;
            }
            Some(Ok(tokio_tungstenite::tungstenite::Message::Close(Some(frame)))) => {
                return Err(format!("Authentication failed: {}", frame.reason));
            }
            Some(Ok(tokio_tungstenite::tungstenite::Message::Close(None))) => {
                return Err("Authentication failed: relay closed connection".into());
            }
            Some(Err(e)) => return Err(format!("WebSocket error: {e}")),
            None => return Err("Relay closed connection before ack".into()),
            _ => continue,
        }
    };

    let public_url = ack.url.clone();
    let local_base = format!("http://localhost:{port}");

    // No HTTP-level timeout on reqwest: we want to stream long-lived SSE
    // responses indefinitely. A `connect_timeout` covers TCP handshake;
    // a per-request `tokio::time::timeout` around `.send()` covers the
    // headers wait separately (HEADERS_TIMEOUT).
    let http_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(10)
        .build()
        .expect("Failed to build tunnel HTTP client");

    status.on_connected(&public_url);

    tokio::spawn(async move {
        let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let in_flight: Arc<DashMap<String, CancellationToken>> = Arc::new(DashMap::new());

        let send_task = tokio::spawn(async move {
            while let Some(msg) = resp_rx.recv().await {
                if ws_sink
                    .send(tokio_tungstenite::tungstenite::Message::Text(msg.into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        let mut evicted = false;
        while let Some(Ok(msg)) = ws_stream.next().await {
            match msg {
                tokio_tungstenite::tungstenite::Message::Text(text) => {
                    match serde_json::from_str::<TunnelMessage>(&text) {
                        Ok(TunnelMessage::Request(req)) => {
                            let client = http_client.clone();
                            let base = local_base.clone();
                            let tx = resp_tx.clone();
                            let in_flight_clone = in_flight.clone();
                            let token = CancellationToken::new();
                            in_flight.insert(req.id.clone(), token.clone());

                            tokio::spawn(async move {
                                let id = req.id.clone();
                                forward_to_local(&client, &base, req, tx, token).await;
                                in_flight_clone.remove(&id);
                            });
                        }
                        Ok(TunnelMessage::Cancel { id }) => {
                            if let Some((_, token)) = in_flight.remove(&id) {
                                token.cancel();
                            }
                        }
                        // Frames not addressed to the tunnel-client side are
                        // silently dropped: defensive against future protocol
                        // additions without forcing a coordinated upgrade.
                        Ok(_) | Err(_) => {}
                    }
                }
                tokio_tungstenite::tungstenite::Message::Close(Some(frame))
                    if frame.code
                        == tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::from(
                            4002,
                        ) =>
                {
                    eprintln!(
                        "  {} tunnel evicted: {}",
                        colored::Colorize::yellow("evicted"),
                        frame.reason
                    );
                    evicted = true;
                    break;
                }
                _ => {}
            }
        }

        if evicted {
            status.on_evicted();
        } else {
            status.on_disconnected();
        }

        send_task.abort();
    });

    Ok(public_url)
}

/// Forward one tunnel request upstream and stream the response back as
/// `ResponseHead` + `ResponseChunk*` + terminator (`last: true`). On
/// upstream error before headers, sends `ResponseError` and returns
/// without a terminator (the relay synthesizes a 502).
async fn forward_to_local(
    client: &reqwest::Client,
    base_url: &str,
    req: TunnelRequest,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
    cancel: CancellationToken,
) {
    let url = format!("{base_url}{}", req.path);
    let method: reqwest::Method = req.method.parse().unwrap_or(reqwest::Method::GET);

    let mut builder = client.request(method, &url);

    for (k, v) in &req.headers {
        if k.eq_ignore_ascii_case("host") {
            continue;
        }
        if is_hop_by_hop(k) {
            continue;
        }
        builder = builder.header(k.as_str(), v.as_str());
    }

    if let Some(body_b64) = &req.body
        && let Ok(body) = base64::engine::general_purpose::STANDARD.decode(body_b64)
    {
        builder = builder.body(body);
    }

    // Headers-only timeout: only bounds the wait for the upstream to
    // produce response headers. Once we have the head, the body streams
    // for as long as the upstream keeps it open.
    let send_fut = builder.send();
    let resp = match tokio::time::timeout(HEADERS_TIMEOUT, send_fut).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            send_msg(
                &tx,
                &TunnelMessage::ResponseError {
                    id: req.id,
                    message: format!("upstream error: {e}"),
                },
            );
            return;
        }
        Err(_) => {
            send_msg(
                &tx,
                &TunnelMessage::ResponseError {
                    id: req.id,
                    message: "upstream headers timeout".into(),
                },
            );
            return;
        }
    };

    let status = resp.status().as_u16();
    let mut headers = HashMap::new();
    for (k, v) in resp.headers() {
        if is_hop_by_hop(k.as_str()) {
            continue;
        }
        if let Ok(val) = v.to_str() {
            headers.insert(k.to_string(), val.to_string());
        }
    }

    if !send_msg(
        &tx,
        &TunnelMessage::ResponseHead {
            id: req.id.clone(),
            status,
            headers,
        },
    ) {
        return;
    }

    let mut stream = resp.bytes_stream();
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            chunk = stream.next() => match chunk {
                Some(Ok(bytes)) => {
                    if !send_chunks(&tx, &req.id, bytes) {
                        return;
                    }
                }
                Some(Err(_)) | None => break,
            },
        }
    }

    // Always send terminator on graceful end. (Cancellation also sends
    // it so the relay can release the response body cleanly; if the WS
    // is gone the send_msg call is a no-op.)
    send_msg(
        &tx,
        &TunnelMessage::ResponseChunk {
            id: req.id,
            data: String::new(),
            last: true,
        },
    );
}

/// Split `bytes` into one or more `ResponseChunk` frames bounded by
/// [`CHUNK_SIZE`] and send them. Returns `false` if the WS sink is gone.
fn send_chunks(tx: &tokio::sync::mpsc::UnboundedSender<String>, id: &str, bytes: Bytes) -> bool {
    if bytes.is_empty() {
        return true;
    }
    let mut offset = 0;
    while offset < bytes.len() {
        let end = (offset + CHUNK_SIZE).min(bytes.len());
        let slice = &bytes[offset..end];
        let msg = TunnelMessage::ResponseChunk {
            id: id.to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(slice),
            last: false,
        };
        if !send_msg(tx, &msg) {
            return false;
        }
        offset = end;
    }
    true
}

fn send_msg(tx: &tokio::sync::mpsc::UnboundedSender<String>, msg: &TunnelMessage) -> bool {
    match serde_json::to_string(msg) {
        Ok(s) => tx.send(s).is_ok(),
        Err(_) => false,
    }
}

fn pick_subdomain(subdomains: &[String]) -> Result<String, String> {
    if subdomains.is_empty() {
        return Err("No subdomains available for this token".into());
    }
    if subdomains.len() == 1 && !subdomains[0].contains('*') {
        return Ok(subdomains[0].clone());
    }

    eprintln!("\nAvailable subdomains:");
    for (i, sub) in subdomains.iter().enumerate() {
        eprintln!("  [{}] {}", i + 1, sub);
    }
    eprint!("Pick a subdomain (number or name): ");

    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("Failed to read input: {e}"))?;
    let input = input.trim();

    if let Ok(n) = input.parse::<usize>()
        && n >= 1
        && n <= subdomains.len()
    {
        let picked = &subdomains[n - 1];
        if picked.contains('*') {
            return Err(format!(
                "'{}' is a wildcard pattern - enter a concrete subdomain name instead",
                picked
            ));
        }
        return Ok(picked.clone());
    }

    Ok(input.to_string())
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;

    use bytes::Bytes;

    fn collect_messages(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    ) -> Vec<TunnelMessage> {
        let mut out = Vec::new();
        while let Ok(s) = rx.try_recv() {
            out.push(serde_json::from_str(&s).expect("valid frame"));
        }
        out
    }

    #[test]
    fn send_chunks__splits_at_chunk_size_boundary() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let body = Bytes::from(vec![0u8; CHUNK_SIZE * 2 + 100]);
        assert!(send_chunks(&tx, "rid", body));
        let msgs = collect_messages(&mut rx);
        assert_eq!(msgs.len(), 3);
        for m in &msgs {
            match m {
                TunnelMessage::ResponseChunk { id, last, .. } => {
                    assert_eq!(id, "rid");
                    assert!(!last);
                }
                _ => panic!("expected ResponseChunk"),
            }
        }
    }

    #[test]
    fn send_chunks__empty_bytes_emits_no_frames() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        assert!(send_chunks(&tx, "rid", Bytes::new()));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn send_chunks__small_bytes_emit_one_frame_round_trips_base64() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        send_chunks(&tx, "rid", Bytes::from_static(b"hello"));
        let msgs = collect_messages(&mut rx);
        assert_eq!(msgs.len(), 1);
        let TunnelMessage::ResponseChunk { data, .. } = &msgs[0] else {
            panic!()
        };
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap();
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn send_msg__returns_false_when_receiver_dropped() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        drop(rx);
        let msg = TunnelMessage::Cancel { id: "x".into() };
        assert!(!send_msg(&tx, &msg));
    }
}
