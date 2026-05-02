# mcp-tunnel

Relay server for [mcpr](https://github.com/pragmalabs-tech/mcpr) tunnels. Accepts WebSocket connections from `mcpr proxy run` instances and routes inbound HTTP traffic to them by subdomain.

```
Internet -> mcp-tunnel (this) -> WebSocket -> mcpr proxy -> MCP server
              tunnel.example.com/myapp                      localhost:3004
```

## Quick start

```sh
docker run -p 8080:8080 ghcr.io/pragmalabs-tech/mcp-tunnel \
  --domain tunnel.example.com
```

On the client side, set `[tunnel]` in your `mcpr.toml`:

```toml
[tunnel]
enabled   = true
relay_url = "https://tunnel.example.com"
token     = "tok_abc"
```

Then run `mcpr proxy run mcpr.toml`. The proxy connects to the relay and prints the public URL:

```
  ready  mcpr proxy running on http://localhost:3004 -> http://localhost:9000
  tunnel public URL: https://myapp.tunnel.example.com
```

## CLI flags

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

Delegates token verification to an external HTTP endpoint. Used with [mcpr cloud](https://mcpr.app) or a custom auth service.

```sh
mcp-tunnel --domain tunnel.example.com \
  --auth-url https://api.mcpr.app \
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
      - --auth-url=https://api.mcpr.app
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

To embed the tunnel client directly into your own MCP server (no separate `mcpr proxy run` process needed), use the [`mcp-tunnel-client`](crates/mcp-tunnel-client) crate published to crates.io:

```toml
[dependencies]
mcp-tunnel-client = "0.1"
```

```rust
use mcp_tunnel_client::start_tunnel_client;

let public_url = start_tunnel_client(9000, "https://tunnel.example.com", "tok_abc", Some("myapp"), MyStatus).await?;
```

See [crates/mcp-tunnel-client/README.md](crates/mcp-tunnel-client/README.md) for the full API.

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

1. `mcpr proxy run` opens a WebSocket to `/_tunnel/register` and sends a registration message with its auth token.
2. The relay authenticates the token, assigns a subdomain, and acknowledges with the public URL.
3. Inbound HTTP requests arrive at `{subdomain}.{domain}`. The relay extracts the subdomain from the `Host` header, finds the matching WebSocket connection, and forwards the request as a JSON message.
4. The proxy receives the request, forwards it to the local MCP server, and sends the response back through the WebSocket.
5. If a second client registers the same subdomain, the relay evicts the previous connection (close code 4002).

## License

Apache-2.0
