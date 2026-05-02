# mcp-tunnel-client

Rust client for [mcp-tunnel](https://github.com/pragmalabs-tech/mcp-tunnel). Embed in your MCP server to expose it through a hosted tunnel relay without running a separate `mcpr proxy`.

```toml
[dependencies]
mcp-tunnel-client = "0.1"
```

## Usage

```rust
use mcp_tunnel_client::{TunnelStatusCallback, start_tunnel_client};

struct Logger;
impl TunnelStatusCallback for Logger {
    fn on_connected(&self, url: &str) { println!("public URL: {url}"); }
    fn on_disconnected(&self)         { println!("disconnected"); }
    fn on_evicted(&self)              { println!("evicted by another client"); }
}

#[tokio::main]
async fn main() -> Result<(), String> {
    // your MCP server is already running on localhost:9000

    let public_url = start_tunnel_client(
        9000,                              // local port
        "https://tunnel.example.com",      // relay URL
        "tok_abc",                         // auth token
        Some("myapp"),                     // requested subdomain (optional)
        Logger,
    ).await?;

    println!("tunnel ready at {public_url}");

    // keep your server alive; the client runs in a background task
    tokio::signal::ctrl_c().await.ok();
    Ok(())
}
```

`start_tunnel_client` opens a WebSocket to the relay, registers, and spawns a background task that forwards inbound HTTP requests to `localhost:port`. It returns the assigned public URL once registration succeeds.

## Auth

The relay decides whether to accept your token. Three modes are supported (configured server-side):

- **open** — any token works; subdomain hashed from the token if not requested.
- **static** — relay is started with a fixed token list; pass the matching token.
- **provider** — relay verifies the token against an external HTTP endpoint.

If the relay returns a list of allowed subdomains, the client either picks the only valid one automatically or prompts on stdin.

## Status callback

`TunnelStatusCallback` fires on connect, disconnect, and eviction (close code 4002, when another client registers the same subdomain). Use it to log, restart, or surface the URL in your UI.

## License

Apache-2.0
