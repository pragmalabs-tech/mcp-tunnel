# mcp-tunnel

Self-hosted HTTP tunnel relay. Run it on a host with a wildcard DNS record, point a client at it from anywhere, and inbound traffic to `{subdomain}.{your-domain}` gets forwarded through a WebSocket to a service running on the client's `localhost`.

```
Internet -> mcp-tunnel relay -> WebSocket -> mcp-tunnel-client -> local HTTP service
            tunnel.example.com                                     localhost:9000
```

Useful for exposing a local dev server, MCP server, webhook receiver, or any HTTP service to the internet without poking holes in firewalls.

## Quick start

Run the relay:

```sh
docker run -p 8080:8080 ghcr.io/pragmalabs-tech/mcp-tunnel \
  --domain tunnel.example.com
```

Connect a client (Rust):

```toml
# Cargo.toml
[dependencies]
mcp-tunnel-client = "0.1"
```

```rust
use mcp_tunnel_client::{TunnelStatusCallback, start_tunnel_client};

struct Logger;
impl TunnelStatusCallback for Logger {
    fn on_connected(&self, url: &str) { println!("public URL: {url}"); }
    fn on_disconnected(&self) {}
    fn on_evicted(&self) {}
}

let public_url = start_tunnel_client(
    9000,                              // your local port
    "https://tunnel.example.com",      // relay URL
    "tok_abc",                         // auth token
    Some("myapp"),                     // requested subdomain
    Logger,
).await?;
```

See [`examples/basic`](examples/basic) for a runnable end-to-end demo.

## Relay CLI flags

```
mcp-tunnel --domain <DOMAIN> [OPTIONS]
```

| Flag | Default | Required | Description |
|---|---|---|---|
| `--domain <DOMAIN>` | - | yes | Base domain for tunnel subdomains |
| `--port <PORT>` | `8080` | no | TCP listen port |
| `--static-token <TOKEN:SUBS>` | - | no | Static token entry; repeatable. Format: `TOKEN:SUBDOMAIN[,SUBDOMAIN...]` |
| `--auth-url <URL>` | - | no | Auth provider base URL (enables provider mode) |
| `--auth-secret <SECRET>` | - | with `--auth-url` | Shared secret sent as `X-Relay-Secret` header |
| `--max-request-body <BYTES>` | `5242880` | no | Max inbound request body in bytes (5 MB) |
| `--max-response-body <BYTES>` | `10485760` | no | Max tunneled response body in bytes (10 MB) |

`--static-token` and `--auth-url` are mutually exclusive. Without either, the relay runs in open mode.

## Auth modes

### Open

No authentication. Any client can register a tunnel. Suitable for private networks or local development.

```sh
mcp-tunnel --domain tunnel.example.com
```

### Static

Token list defined at startup via repeated `--static-token` flags. Each entry maps a token to a list of allowed subdomain patterns.

```sh
mcp-tunnel --domain tunnel.example.com \
  --static-token tok_abc:myapp,myapp-* \
  --static-token tok_xyz:other-app
```

Subdomain patterns support a single `*` wildcard:

| Pattern | Matches |
|---|---|
| `myapp` | exactly `myapp` |
| `myapp-*` | `myapp-dev`, `myapp-feat-123` |
| `*-preview` | `feat-preview`, `hotfix-preview` |
| `pr-*-corp` | `pr-123-corp`, `pr-abc-corp` |
| `*` | anything |

### Provider

Delegates token verification to an external HTTP endpoint (run your own auth service).

```sh
mcp-tunnel --domain tunnel.example.com \
  --auth-url https://auth.example.com \
  --auth-secret <shared-secret>
```

The relay calls `POST {auth-url}/api/verify` with:

```json
{ "token": "tok_abc", "subdomain": "myapp" }
```

Header: `X-Relay-Secret: <auth-secret>`

Expected response:

```json
{ "subdomains": ["myapp", "myapp-*"] }
```

Return `401`/`403` for invalid tokens, `5xx` for transient errors (client gets "auth provider unavailable").

## Docker Compose example

```yaml
services:
  mcp-tunnel:
    image: ghcr.io/pragmalabs-tech/mcp-tunnel:latest
    restart: unless-stopped
    ports:
      - "8080:8080"
    command:
      - --domain=tunnel.example.com
      - --auth-url=https://auth.example.com
      - --auth-secret=${RELAY_SECRET}
```

Traffic must reach the container with the correct `Host` header. Sit a reverse proxy (nginx, Caddy, Traefik) in front and route `*.tunnel.example.com` to port 8080.

### nginx example

```nginx
server {
    listen 443 ssl;
    server_name *.tunnel.example.com;

    ssl_certificate     /etc/ssl/tunnel.example.com/fullchain.pem;
    ssl_certificate_key /etc/ssl/tunnel.example.com/privkey.pem;

    location / {
        proxy_pass         http://localhost:8080;
        proxy_http_version 1.1;
        proxy_set_header   Upgrade $http_upgrade;
        proxy_set_header   Connection "upgrade";
        proxy_set_header   Host $host;
        proxy_read_timeout 60s;
    }
}
```

The wildcard TLS certificate covers `*.tunnel.example.com`. Let's Encrypt supports wildcard certs via DNS-01 challenge.

## Client library

To connect a service from Rust, use [`mcp-tunnel-client`](crates/mcp-tunnel-client) — published to crates.io.

### Install

```sh
cargo new --bin mytunnel
cd mytunnel
cargo add mcp-tunnel-client
cargo add tokio --features macros,rt-multi-thread,signal
cargo add axum                                 # or whatever HTTP framework you use
```

### Use

`src/main.rs`:

```rust
use axum::{Router, routing::get};
use mcp_tunnel_client::{TunnelStatusCallback, start_tunnel_client};

struct Logger;
impl TunnelStatusCallback for Logger {
    fn on_connected(&self, url: &str) { println!("public URL: {url}"); }
    fn on_disconnected(&self) { println!("disconnected"); }
    fn on_evicted(&self) { println!("evicted"); }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. start your local HTTP service
    let app = Router::new().route("/", get(|| async { "hello" }));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 9000)).await?;
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });

    // 2. expose it through the relay
    let public_url = start_tunnel_client(
        9000,                              // local port your service listens on
        "https://tunnel.example.com",      // relay URL
        "tok_abc",                         // auth token (use "any" in open mode)
        Some("myapp"),                     // requested subdomain (None lets the relay pick)
        Logger,
    ).await?;
    println!("reachable at {public_url}");

    // 3. keep the process alive; the tunnel runs in a background task
    tokio::signal::ctrl_c().await?;
    Ok(())
}
```

```sh
cargo run
```

See [`examples/basic`](examples/basic) for a runnable end-to-end demo and [crates/mcp-tunnel-client/README.md](crates/mcp-tunnel-client/README.md) for the full API.

## Build from source

Requires Rust 1.92+. This is a Cargo workspace with two crates:

- `crates/mcp-tunnel` — the relay binary (this is what runs in the Docker image)
- `crates/mcp-tunnel-client` — the client library, published to crates.io

```sh
cargo build --release
./target/release/mcp-tunnel --domain tunnel.example.com
```

## Releasing

Releases are driven by [`cargo-release`](https://github.com/crate-ci/cargo-release):

```sh
cargo install cargo-release             # one-time
cargo release minor                     # dry run
cargo release minor --execute           # bump, commit, tag, publish, push
```

This bumps both crates to the same version, publishes `mcp-tunnel-client` to crates.io (the binary is `publish = false`), tags `vX.Y.Z`, and pushes. GitHub Actions then sees the tag and builds/pushes the multi-arch Docker image to ghcr.io.

## How it works

1. The client opens a WebSocket to `/_tunnel/register` and sends a registration message with its auth token and (optional) requested subdomain.
2. The relay verifies the token, assigns a subdomain, and acknowledges with the public URL.
3. Inbound HTTP requests arrive at `{subdomain}.{domain}`. The relay extracts the subdomain from the `Host` header, finds the matching WebSocket connection, and forwards the request as a JSON message (base64 body).
4. The client receives the request, forwards it to the local service, and sends the response back through the WebSocket.
5. If a second client registers the same subdomain, the relay evicts the previous connection (close code 4002).

## License

Apache-2.0
