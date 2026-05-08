use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::DefaultBodyLimit;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use base64::Engine;
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};

use mcp_tunnel_client::protocol::{
    RegisterAck, RegisterRequest, SubdomainOffer, SubdomainPick, TunnelMessage, TunnelRequest,
    is_hop_by_hop,
};

use crate::auth::{AuthError, AuthProviderConfig, subdomain_matches, verify_token};
use crate::config::RelayConfig;

/// Bytes the body channel buffers before backpressuring the WS recv
/// task. Smaller = stronger backpressure but more context switches.
const BODY_CHANNEL_DEPTH: usize = 32;

/// Headers-only timeout. Matches the tunnel-client side. Body streams
/// indefinitely once head arrives.
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-request state tracked by the relay between sending `Request` to
/// the tunnel-client and the matching response stream completing.
enum InFlight {
    /// Waiting for `ResponseHead`. The oneshot fires with the head and
    /// the receiver end of a fresh body channel.
    AwaitingHead(oneshot::Sender<HeadAndBody>),
    /// Head delivered. Subsequent `ResponseChunk` frames are pushed
    /// into this sender. When the matching receiver is dropped (axum
    /// finished or the public client disconnected) the sender errors,
    /// the cancel watcher emits `Cancel`, and this entry is removed.
    Streaming(mpsc::Sender<Result<Bytes, std::io::Error>>),
}

struct HeadAndBody {
    status: u16,
    headers: HashMap<String, String>,
    body_rx: mpsc::Receiver<Result<Bytes, std::io::Error>>,
}

type PendingRequests = Arc<DashMap<String, InFlight>>;
type TunnelSender = tokio::sync::mpsc::Sender<String>;

struct TunnelConnection {
    sender: TunnelSender,
    pending: PendingRequests,
    evict: tokio::sync::Notify,
}

struct RelayState {
    tunnels: DashMap<String, Arc<TunnelConnection>>,
    base_domain: String,
    auth: AuthMode,
}

enum AuthMode {
    Open,
    Static(HashMap<String, Vec<String>>),
    Provider(AuthProviderConfig),
}

fn token_to_subdomain(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let hash = hasher.finalize();
    hash[..6].iter().map(|b| format!("{b:02x}")).collect()
}

/// Build the relay Axum app without binding or serving.
/// Returns `(router, port)` - the caller controls the TCP listener and shutdown.
pub fn build_relay_app(cfg: RelayConfig) -> (Router, u16) {
    let auth = if !cfg.tokens.is_empty() {
        let count = cfg.tokens.len();
        println!(
            "  {} static tokens: {} token(s) configured",
            colored::Colorize::green("ready"),
            count,
        );
        AuthMode::Static(cfg.tokens)
    } else if let Some(url) = cfg.auth_provider {
        let secret = cfg
            .auth_provider_secret
            .expect("auth_provider_secret is required when auth_provider is set");
        println!(
            "  {} auth provider: {url}",
            colored::Colorize::green("ready")
        );
        AuthMode::Provider(AuthProviderConfig {
            url: url.trim_end_matches('/').to_string(),
            secret,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap(),
        })
    } else {
        println!(
            "  {} open mode (anyone can tunnel)",
            colored::Colorize::yellow("warn"),
        );
        AuthMode::Open
    };

    let state = Arc::new(RelayState {
        tunnels: DashMap::new(),
        base_domain: cfg.relay_domain,
        auth,
    });

    const DEFAULT_MAX_REQUEST_BODY: usize = 5 * 1024 * 1024;
    let max_body = cfg
        .max_request_body_size
        .unwrap_or(DEFAULT_MAX_REQUEST_BODY);

    let app = Router::new()
        .route("/_tunnel/register", any(handle_register))
        .fallback(any(handle_tunnel_request))
        .with_state(state)
        .layer(DefaultBodyLimit::max(max_body));

    (app, cfg.port)
}

async fn handle_register(ws: WebSocketUpgrade, State(state): State<Arc<RelayState>>) -> Response {
    ws.on_upgrade(move |socket| handle_tunnel_ws(socket, state))
}

async fn handle_tunnel_ws(socket: WebSocket, state: Arc<RelayState>) {
    let (mut ws_sink, mut ws_stream) = socket.split();

    let reg: RegisterRequest = loop {
        match ws_stream.next().await {
            Some(Ok(Message::Text(text))) => match serde_json::from_str(&text) {
                Ok(req) => break req,
                Err(_) => continue,
            },
            Some(Err(_)) | None => return,
            _ => continue,
        }
    };

    let token = reg.token;
    let requested_subdomain = reg.subdomain;

    async fn close_with_error(
        ws_sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
        reason: &str,
    ) {
        let _ = ws_sink
            .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                code: 4001,
                reason: reason.into(),
            })))
            .await;
    }

    async fn offer_and_pick(
        ws_sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
        ws_stream: &mut futures_util::stream::SplitStream<WebSocket>,
        allowed: &[String],
    ) -> Option<String> {
        let offer = SubdomainOffer {
            subdomains: allowed.to_vec(),
        };
        if ws_sink
            .send(Message::Text(serde_json::to_string(&offer).unwrap().into()))
            .await
            .is_err()
        {
            return None;
        }
        loop {
            match ws_stream.next().await {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(pick) = serde_json::from_str::<SubdomainPick>(&text) {
                        return Some(pick.subdomain);
                    }
                }
                _ => return None,
            }
        }
    }

    async fn resolve_subdomain(
        requested: Option<String>,
        allowed: &[String],
        ws_sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
        ws_stream: &mut futures_util::stream::SplitStream<WebSocket>,
    ) -> Option<String> {
        if let Some(ref sub) = requested
            && subdomain_matches(allowed, sub)
        {
            return Some(sub.clone());
        }
        if allowed.len() == 1 && !allowed[0].contains('*') {
            return Some(allowed[0].clone());
        }
        let picked = offer_and_pick(ws_sink, ws_stream, allowed).await?;
        if subdomain_matches(allowed, &picked) {
            Some(picked)
        } else {
            close_with_error(
                ws_sink,
                &format!("subdomain '{}' not authorized for this token", picked),
            )
            .await;
            None
        }
    }

    let subdomain = match &state.auth {
        AuthMode::Open => Some(requested_subdomain.unwrap_or_else(|| token_to_subdomain(&token))),
        AuthMode::Static(tokens) => match tokens.get(&token) {
            Some(allowed) => {
                resolve_subdomain(requested_subdomain, allowed, &mut ws_sink, &mut ws_stream).await
            }
            None => {
                close_with_error(&mut ws_sink, "invalid token").await;
                return;
            }
        },
        AuthMode::Provider(auth) => {
            let sub_for_verify = requested_subdomain.as_deref().unwrap_or("");
            match verify_token(auth, &token, sub_for_verify).await {
                Ok(allowed) => {
                    resolve_subdomain(requested_subdomain, &allowed, &mut ws_sink, &mut ws_stream)
                        .await
                }
                Err(AuthError::InvalidToken(msg)) => {
                    close_with_error(&mut ws_sink, &msg).await;
                    return;
                }
                Err(AuthError::ProviderUnavailable(msg)) => {
                    eprintln!(
                        "  {} auth provider error: {msg}",
                        colored::Colorize::red("error")
                    );
                    close_with_error(&mut ws_sink, "auth provider unavailable").await;
                    return;
                }
            }
        }
    };

    let subdomain = match subdomain {
        Some(s) => s,
        None => return,
    };

    let url = format!("https://{}.{}", subdomain, state.base_domain);

    let ack = RegisterAck {
        subdomain: subdomain.clone(),
        url: url.clone(),
    };
    if ws_sink
        .send(Message::Text(serde_json::to_string(&ack).unwrap().into()))
        .await
        .is_err()
    {
        return;
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(64);
    let pending: PendingRequests = Arc::new(DashMap::new());
    let conn = Arc::new(TunnelConnection {
        sender: tx,
        pending: pending.clone(),
        evict: tokio::sync::Notify::new(),
    });

    if let Some((_, old)) = state.tunnels.remove(&subdomain) {
        old.evict.notify_one();
        println!(
            "  {} evicted old tunnel: {subdomain}",
            colored::Colorize::yellow("warn")
        );
    }

    let conn_for_evict = conn.clone();
    let conn_for_dispatch = conn.clone();
    state.tunnels.insert(subdomain.clone(), conn);
    println!(
        "  {} tunnel registered: {}",
        colored::Colorize::green("ready"),
        colored::Colorize::cyan(url.as_str())
    );

    let send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        Some(msg) => {
                            if ws_sink.send(Message::Text(msg.into())).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                () = conn_for_evict.evict.notified() => {
                    let _ = ws_sink
                        .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                            code: 4002,
                            reason: "evicted: another client registered with the same tunnel".into(),
                        })))
                        .await;
                    break;
                }
            }
        }
    });

    while let Some(Ok(msg)) = ws_stream.next().await {
        if let Message::Text(text) = msg {
            dispatch_frame(&text, &conn_for_dispatch).await;
        }
    }

    send_task.abort();
    state.tunnels.remove(&subdomain);
    println!(
        "  {} tunnel disconnected: {subdomain}",
        colored::Colorize::red("error")
    );
}

/// Route an incoming WS text frame from the tunnel-client to the right
/// per-request channel. If a chunk send fails because the public client
/// has disconnected, send Cancel to the tunnel-client and clean up.
async fn dispatch_frame(text: &str, conn: &Arc<TunnelConnection>) {
    let pending = &conn.pending;
    let msg = match serde_json::from_str::<TunnelMessage>(text) {
        Ok(m) => m,
        Err(_) => return,
    };
    match msg {
        TunnelMessage::ResponseHead {
            id,
            status,
            headers,
        } => {
            // Replace AwaitingHead with Streaming. If the entry is missing
            // (cancelled or unknown id), drop the message.
            let Some((_, slot)) = pending.remove(&id) else {
                return;
            };
            let InFlight::AwaitingHead(head_tx) = slot else {
                return;
            };
            let (body_tx, body_rx) =
                mpsc::channel::<Result<Bytes, std::io::Error>>(BODY_CHANNEL_DEPTH);
            pending.insert(id, InFlight::Streaming(body_tx));
            let _ = head_tx.send(HeadAndBody {
                status,
                headers,
                body_rx,
            });
        }
        TunnelMessage::ResponseChunk { id, data, last } => {
            // Take a clone of the Sender so we can drop the dashmap guard
            // before awaiting send. Cloning here (locally) is fine because
            // the clone lives only until this match arm returns; the only
            // long-lived Sender stays in `pending` and is dropped by the
            // terminator path below.
            let tx_clone = {
                let Some(slot) = pending.get(&id) else { return };
                let InFlight::Streaming(tx) = slot.value() else {
                    return;
                };
                tx.clone()
            };
            let mut send_failed = false;
            if !data.is_empty()
                && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&data)
                && tx_clone.send(Ok(Bytes::from(bytes))).await.is_err()
            {
                // Public client gone (axum dropped the body Receiver).
                // Cancel the upstream stream and clean up.
                send_failed = true;
            }
            drop(tx_clone);

            if send_failed {
                if pending.remove(&id).is_some() {
                    send_cancel(conn, &id).await;
                }
            } else if last {
                pending.remove(&id);
            }
        }
        TunnelMessage::ResponseError { id, message } => {
            let Some((_, slot)) = pending.remove(&id) else {
                return;
            };
            match slot {
                InFlight::AwaitingHead(head_tx) => {
                    let (synth_tx, body_rx) = mpsc::channel::<Result<Bytes, _>>(1);
                    let body_msg = format!("upstream error: {message}").into_bytes();
                    let _ = synth_tx.send(Ok(Bytes::from(body_msg))).await;
                    drop(synth_tx);
                    let _ = head_tx.send(HeadAndBody {
                        status: 502,
                        headers: HashMap::new(),
                        body_rx,
                    });
                }
                InFlight::Streaming(tx) => {
                    let _ = tx.send(Err(std::io::Error::other(message))).await;
                }
            }
        }
        TunnelMessage::Request(_) | TunnelMessage::Cancel { .. } => {
            // Frames sent the wrong direction; ignore.
        }
    }
}

async fn handle_tunnel_request(
    State(state): State<Arc<RelayState>>,
    method: Method,
    headers: HeaderMap,
    uri: axum::http::Uri,
    body: Bytes,
) -> Response {
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let subdomain = host.split('.').next().unwrap_or("").to_string();
    let path_str = uri
        .path_and_query()
        .map(|p| p.to_string())
        .unwrap_or_else(|| "/".into());

    if subdomain.is_empty() {
        relay_log(
            "-",
            method.as_str(),
            &path_str,
            400,
            std::time::Duration::ZERO,
        );
        return (StatusCode::BAD_REQUEST, "missing host header").into_response();
    }

    let conn = match state.tunnels.get(&subdomain) {
        Some(c) => c.clone(),
        None => {
            relay_log(
                &subdomain,
                method.as_str(),
                &path_str,
                502,
                std::time::Duration::ZERO,
            );
            return (StatusCode::BAD_GATEWAY, "tunnel not found").into_response();
        }
    };

    let req_id = uuid::Uuid::new_v4().to_string();
    let mut req_headers = HashMap::new();
    for (key, val) in headers.iter() {
        if is_hop_by_hop(key.as_str()) {
            continue;
        }
        if let Ok(v) = val.to_str() {
            req_headers.insert(key.to_string(), v.to_string());
        }
    }

    let body_b64 = if body.is_empty() {
        None
    } else {
        Some(base64::engine::general_purpose::STANDARD.encode(&body))
    };

    let tunnel_req = TunnelRequest {
        id: req_id.clone(),
        method: method.to_string(),
        path: path_str.clone(),
        headers: req_headers,
        body: body_b64,
    };

    let (head_tx, head_rx) = oneshot::channel::<HeadAndBody>();
    conn.pending
        .insert(req_id.clone(), InFlight::AwaitingHead(head_tx));

    let frame = serde_json::to_string(&TunnelMessage::Request(tunnel_req)).unwrap();
    match conn.sender.try_send(frame) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            conn.pending.remove(&req_id);
            relay_log(
                &subdomain,
                method.as_str(),
                &path_str,
                503,
                std::time::Duration::ZERO,
            );
            return (StatusCode::SERVICE_UNAVAILABLE, "tunnel overloaded").into_response();
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            conn.pending.remove(&req_id);
            relay_log(
                &subdomain,
                method.as_str(),
                &path_str,
                502,
                std::time::Duration::ZERO,
            );
            return (StatusCode::BAD_GATEWAY, "tunnel disconnected").into_response();
        }
    }

    let start = std::time::Instant::now();

    let head = match tokio::time::timeout(HEAD_TIMEOUT, head_rx).await {
        Ok(Ok(h)) => h,
        Ok(Err(_)) => {
            conn.pending.remove(&req_id);
            relay_log(&subdomain, method.as_str(), &path_str, 502, start.elapsed());
            return (StatusCode::BAD_GATEWAY, "tunnel dropped request").into_response();
        }
        Err(_) => {
            conn.pending.remove(&req_id);
            send_cancel(&conn, &req_id).await;
            relay_log(&subdomain, method.as_str(), &path_str, 504, start.elapsed());
            return (StatusCode::GATEWAY_TIMEOUT, "tunnel headers timeout").into_response();
        }
    };

    relay_log(
        &subdomain,
        method.as_str(),
        &path_str,
        head.status,
        start.elapsed(),
    );

    // Build the streaming response. The body channel keeps draining as
    // long as the response body is alive on the public client side.
    let mut builder = Response::builder()
        .status(StatusCode::from_u16(head.status).unwrap_or(StatusCode::BAD_GATEWAY));
    for (k, v) in &head.headers {
        if is_hop_by_hop(k) {
            continue;
        }
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            builder = builder.header(name, val);
        }
    }

    // Cancellation is handled inside dispatch_frame's ResponseChunk arm:
    // when a chunk send fails (axum dropped the body Receiver), we send
    // Cancel and remove the pending entry. No separate watcher needed.

    let body_stream = tokio_stream::wrappers::ReceiverStream::new(head.body_rx);
    let body = axum::body::Body::from_stream(body_stream);
    builder.body(body).unwrap_or_else(|_| {
        (StatusCode::INTERNAL_SERVER_ERROR, "response build error").into_response()
    })
}

async fn send_cancel(conn: &TunnelConnection, id: &str) {
    let frame = serde_json::to_string(&TunnelMessage::Cancel { id: id.to_string() }).unwrap();
    let _ = conn.sender.try_send(frame);
}

fn relay_log(
    subdomain: &str,
    method: &str,
    path: &str,
    status: u16,
    duration: std::time::Duration,
) {
    use colored::Colorize;
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let status_str = if status < 300 {
        format!("{status}").green().to_string()
    } else if status < 400 {
        format!("{status}").yellow().to_string()
    } else {
        format!("{status}").red().to_string()
    };
    println!(
        "  {now}  {sub}  {method} {path}  -> {status}  {ms}ms",
        sub = subdomain.dimmed(),
        status = status_str,
        ms = duration.as_millis(),
    );
}
