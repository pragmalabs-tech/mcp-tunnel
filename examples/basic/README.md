# basic

Expose a local axum server through a self-hosted tunnel relay using `mcp-tunnel-client`.

## 1. Run the relay (Docker)

```sh
docker run --rm -p 8080:8080 ghcr.io/pragmalabs-tech/mcp-tunnel \
  --domain localhost.test
```

The relay listens on `:8080` with no auth (open mode). Host-header routing means `Host: myapp.localhost.test` lands on the tunnel registered as `myapp`.

> For real deployments use `--static-token` or `--auth-url` and put a TLS-terminating reverse proxy in front. See the top-level README.

## 2. Run the example client

```sh
cd examples/basic
cargo run
```

You should see:

```
[server] listening on http://127.0.0.1:9000
[tunnel] connected: https://myapp.localhost.test
[server] reachable at https://myapp.localhost.test
```

## 3. Send traffic to the tunnel

The relay routes by `Host`. Hit it directly:

```sh
curl -H 'Host: myapp.localhost.test' http://localhost:8080/
# hello from a tunneled service

curl -H 'Host: myapp.localhost.test' http://localhost:8080/health
# ok
```

In production the relay sits behind nginx/Caddy with a wildcard cert, so you'd just `curl https://myapp.tunnel.example.com/`.

## What this shows

- `start_tunnel_client(port, relay_url, token, subdomain, callback)` opens a WebSocket to the relay, registers, and spawns a background task that forwards inbound HTTP requests to your local server.
- The function returns the assigned public URL.
- `TunnelStatusCallback` fires on connect / disconnect / eviction so you can log or react in your UI.

That's the entire integration - 4 imports and one function call.

## Using in your own crate

```toml
[dependencies]
mcp-tunnel-client = "0.1"
```

(this example uses a `path =` dep so it builds inside the workspace)
