use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// RFC 7230 hop-by-hop headers plus `content-length`.
///
/// These must not be forwarded across a proxy: they describe the
/// connection between the current pair of peers, not the end-to-end
/// message. Forwarding `transfer-encoding` or a stale `content-length`
/// through the tunnel confuses hyper's body framing on the other side
/// and causes the TCP connection to be dropped before the response is
/// serialized, which surfaces to the client as a 502 with
/// "upstream prematurely closed connection".
pub fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "content-length"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// Inbound HTTP request to forward upstream. Body is fully buffered:
/// MCP request bodies are small JSON-RPC envelopes, so streamed
/// *requests* are out of scope for the v0.2 protocol. Streamed
/// responses are the critical case (see [`TunnelMessage::ResponseChunk`]).
#[derive(Serialize, Deserialize, Clone)]
pub struct TunnelRequest {
    pub id: String,
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Option<String>, // base64
}

/// All frames carried over the WebSocket between relay and tunnel-client.
///
/// Per request id, the lifecycle is:
///   relay → client: `Request`
///   client → relay: `ResponseHead`, then 0+ `ResponseChunk` (last one
///     `last: true`), or a single `ResponseError` if the upstream errored
///     before producing headers.
///   relay → client (any time before terminator): `Cancel` to abort the
///     in-flight upstream request (e.g. the public client disconnected).
///
/// Frames not matching the expected direction are ignored (defensive).
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TunnelMessage {
    /// Relay → Client. Forward this request to the local upstream.
    Request(TunnelRequest),

    /// Client → Relay. Upstream returned headers; body chunks follow.
    ResponseHead {
        id: String,
        status: u16,
        headers: HashMap<String, String>,
    },

    /// Client → Relay. One chunk of the response body. `data` is base64.
    /// `last: true` signals end of stream (graceful close); after this the
    /// id is consumed and any further frames for it are ignored.
    ResponseChunk {
        id: String,
        data: String,
        last: bool,
    },

    /// Client → Relay. Upstream errored before producing headers (DNS
    /// failure, connect refused, headers timeout). The relay synthesizes
    /// a 502 response.
    ResponseError { id: String, message: String },

    /// Relay → Client. Cancel an in-flight request. The tunnel-client
    /// aborts the upstream connection. No more frames will be sent for
    /// this id; the client should not emit a terminator either.
    Cancel { id: String },
}

#[derive(Serialize, Deserialize)]
pub struct RegisterRequest {
    pub token: String,
    #[serde(default)]
    pub subdomain: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct RegisterAck {
    pub subdomain: String,
    pub url: String,
}

/// Sent by relay when client didn't specify a subdomain and auth returned an allowed list.
#[derive(Serialize, Deserialize)]
pub struct SubdomainOffer {
    pub subdomains: Vec<String>,
}

/// Sent by client to pick a subdomain from the offered list.
#[derive(Serialize, Deserialize)]
pub struct SubdomainPick {
    pub subdomain: String,
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;

    use serde_json::json;

    #[test]
    fn is_hop_by_hop__flags_all_rfc7230_headers() {
        for h in [
            "connection",
            "keep-alive",
            "proxy-authenticate",
            "proxy-authorization",
            "te",
            "trailer",
            "transfer-encoding",
            "upgrade",
        ] {
            assert!(is_hop_by_hop(h), "{h} should be hop-by-hop");
        }
    }

    #[test]
    fn is_hop_by_hop__flags_content_length() {
        assert!(is_hop_by_hop("content-length"));
    }

    #[test]
    fn is_hop_by_hop__is_case_insensitive() {
        assert!(is_hop_by_hop("Transfer-Encoding"));
        assert!(is_hop_by_hop("TRANSFER-ENCODING"));
        assert!(is_hop_by_hop("Content-Length"));
    }

    #[test]
    fn is_hop_by_hop__allows_end_to_end_headers() {
        for h in [
            "content-type",
            "content-encoding",
            "cache-control",
            "mcp-session-id",
            "authorization",
            "host",
            "accept",
            "user-agent",
            "set-cookie",
        ] {
            assert!(!is_hop_by_hop(h), "{h} should NOT be hop-by-hop");
        }
    }

    #[test]
    fn is_hop_by_hop__rejects_empty() {
        assert!(!is_hop_by_hop(""));
    }

    // ── TunnelMessage serde ──────────────────────────────────────

    #[test]
    fn tunnel_message__request_serializes_with_kind_request() {
        let msg = TunnelMessage::Request(TunnelRequest {
            id: "abc".into(),
            method: "GET".into(),
            path: "/".into(),
            headers: HashMap::new(),
            body: None,
        });
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(v["kind"], "request");
        assert_eq!(v["id"], "abc");
        assert_eq!(v["method"], "GET");
    }

    #[test]
    fn tunnel_message__response_head_kind_and_fields() {
        let mut headers = HashMap::new();
        headers.insert("content-type".into(), "text/event-stream".into());
        let msg = TunnelMessage::ResponseHead {
            id: "rid".into(),
            status: 200,
            headers,
        };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(v["kind"], "response_head");
        assert_eq!(v["id"], "rid");
        assert_eq!(v["status"], 200);
        assert_eq!(v["headers"]["content-type"], "text/event-stream");
    }

    #[test]
    fn tunnel_message__response_chunk_kind_and_fields() {
        let msg = TunnelMessage::ResponseChunk {
            id: "rid".into(),
            data: "aGVsbG8=".into(), // "hello"
            last: false,
        };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(v["kind"], "response_chunk");
        assert_eq!(v["data"], "aGVsbG8=");
        assert_eq!(v["last"], false);
    }

    #[test]
    fn tunnel_message__response_chunk_terminator_serializes_with_last_true() {
        let msg = TunnelMessage::ResponseChunk {
            id: "rid".into(),
            data: String::new(),
            last: true,
        };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(v["last"], true);
        assert_eq!(v["data"], "");
    }

    #[test]
    fn tunnel_message__response_error_kind_carries_message() {
        let msg = TunnelMessage::ResponseError {
            id: "rid".into(),
            message: "connect refused".into(),
        };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(v["kind"], "response_error");
        assert_eq!(v["message"], "connect refused");
    }

    #[test]
    fn tunnel_message__cancel_kind_carries_id() {
        let msg = TunnelMessage::Cancel { id: "rid".into() };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(v["kind"], "cancel");
        assert_eq!(v["id"], "rid");
    }

    #[test]
    fn tunnel_message__deserializes_each_kind() {
        for (raw, want_id) in [
            (
                json!({ "kind": "request", "id": "1", "method": "GET", "path": "/", "headers": {} }),
                "1",
            ),
            (
                json!({ "kind": "response_head", "id": "2", "status": 200, "headers": {} }),
                "2",
            ),
            (
                json!({ "kind": "response_chunk", "id": "3", "data": "", "last": true }),
                "3",
            ),
            (
                json!({ "kind": "response_error", "id": "4", "message": "x" }),
                "4",
            ),
            (json!({ "kind": "cancel", "id": "5" }), "5"),
        ] {
            let s = serde_json::to_string(&raw).unwrap();
            let msg: TunnelMessage = serde_json::from_str(&s).expect("deserialize");
            let id = match &msg {
                TunnelMessage::Request(r) => r.id.as_str(),
                TunnelMessage::ResponseHead { id, .. } => id,
                TunnelMessage::ResponseChunk { id, .. } => id,
                TunnelMessage::ResponseError { id, .. } => id,
                TunnelMessage::Cancel { id } => id,
            };
            assert_eq!(id, want_id);
        }
    }

    #[test]
    fn tunnel_message__rejects_unknown_kind() {
        let s = r#"{"kind":"something_made_up","id":"x"}"#;
        assert!(serde_json::from_str::<TunnelMessage>(s).is_err());
    }
}
