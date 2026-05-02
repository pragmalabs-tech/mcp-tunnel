# mcp-tunnel

Relay server for [mcpr](https://github.com/pragmalabs-tech/mcpr) tunnels. Accepts WebSocket connections from `mcpr proxy run` instances and routes inbound HTTP traffic to them by subdomain.

```
Internet -> mcp-tunnel (this) -> WebSocket -> mcpr proxy -> MCP server
              tunnel.example.com/myapp                      localhost:3004
```

## Quick start

```sh
docker run \
  -e MCPR_RELAY_DOMAIN=tunnel.example.com \
  -p 8080:8080 \
  ghcr.io/pragmalabs-tech/mcp-tunnel
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

## Configuration

All configuration is via environment variables.

| Variable | Default | Required | Description |
|---|---|---|---|
| `MCPR_RELAY_DOMAIN` | - | yes | Base domain for tunnel subdomains |
| `MCPR_RELAY_PORT` | `8080` | no | TCP listen port |
| `MCPR_RELAY_AUTH_MODE` | `open` | no | Auth mode: `open`, `static`, or `provider` |
| `MCPR_RELAY_TOKENS` | - | if `static` | JSON array of token entries (see below) |
| `MCPR_RELAY_AUTH_URL` | - | if `provider` | Auth provider base URL |
| `MCPR_RELAY_AUTH_SECRET` | - | if `provider` | Shared secret sent as `X-Relay-Secret` header |
| `MCPR_RELAY_MAX_REQUEST_BODY` | `5242880` | no | Max inbound request body in bytes (5 MB) |
| `MCPR_RELAY_MAX_RESPONSE_BODY` | `10485760` | no | Max tunneled response body in bytes (10 MB) |

## Auth modes

### open

No authentication. Any client can register a tunnel. Suitable for private networks or local development.

```sh
MCPR_RELAY_AUTH_MODE=open
```

### static

Token list defined at startup via `MCPR_RELAY_TOKENS`. Each entry maps a token to a list of allowed subdomain patterns.

```sh
MCPR_RELAY_AUTH_MODE=static
MCPR_RELAY_TOKENS='[{"token":"tok_abc","subdomains":["myapp","myapp-*"]}]'
```

Subdomain patterns support a single `*` wildcard:

| Pattern | Matches |
|---|---|
| `myapp` | exactly `myapp` |
| `myapp-*` | `myapp-dev`, `myapp-feat-123` |
| `*-preview` | `feat-preview`, `hotfix-preview` |
| `pr-*-corp` | `pr-123-corp`, `pr-abc-corp` |
| `*` | anything |

### provider

Delegates token verification to an external HTTP endpoint. Used with [mcpr cloud](https://mcpr.app) or a custom auth service.

```sh
MCPR_RELAY_AUTH_MODE=provider
MCPR_RELAY_AUTH_URL=https://api.mcpr.app
MCPR_RELAY_AUTH_SECRET=<shared-secret>
```

The relay calls `POST {MCPR_RELAY_AUTH_URL}/api/verify` with:

```json
{ "token": "tok_abc", "subdomain": "myapp" }
```

Header: `X-Relay-Secret: <MCPR_RELAY_AUTH_SECRET>`

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
    environment:
      MCPR_RELAY_DOMAIN: tunnel.example.com
      MCPR_RELAY_AUTH_MODE: provider
      MCPR_RELAY_AUTH_URL: https://api.mcpr.app
      MCPR_RELAY_AUTH_SECRET: ${RELAY_SECRET}
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

## Build from source

Requires Rust 1.92+.

```sh
cargo build --release
./target/release/mcp-tunnel
```

## How it works

1. `mcpr proxy run` opens a WebSocket to `/_tunnel/register` and sends a registration message with its auth token.
2. The relay authenticates the token, assigns a subdomain, and acknowledges with the public URL.
3. Inbound HTTP requests arrive at `{subdomain}.{MCPR_RELAY_DOMAIN}`. The relay extracts the subdomain from the `Host` header, finds the matching WebSocket connection, and forwards the request as a JSON message.
4. The proxy receives the request, forwards it to the local MCP server, and sends the response back through the WebSocket.
5. If a second client registers the same subdomain, the relay evicts the previous connection (close code 4002).

## License

Apache-2.0
