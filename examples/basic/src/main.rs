//! Run a local axum server and expose it through an mcp-tunnel relay.
//!
//! See README.md for the full walkthrough (start the relay with `docker run`,
//! then run this binary).

use axum::{Router, routing::get};
use mcp_tunnel_client::{TunnelStatusCallback, start_tunnel_client};

const RELAY_URL: &str = "http://localhost:8080";
const TOKEN: &str = "dev";
const SUBDOMAIN: &str = "myapp";
const LOCAL_PORT: u16 = 9000;

struct Logger;
impl TunnelStatusCallback for Logger {
    fn on_connected(&self, url: &str) {
        println!("[tunnel] connected: {url}");
    }
    fn on_disconnected(&self) {
        println!("[tunnel] disconnected");
    }
    fn on_evicted(&self) {
        println!("[tunnel] evicted (another client took the subdomain)");
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = Router::new()
        .route("/", get(|| async { "hello from a tunneled service\n" }))
        .route("/health", get(|| async { "ok" }));

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", LOCAL_PORT)).await?;
    println!("[server] listening on http://127.0.0.1:{LOCAL_PORT}");

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let public_url =
        start_tunnel_client(LOCAL_PORT, RELAY_URL, TOKEN, Some(SUBDOMAIN), Logger).await?;
    println!("[server] reachable at {public_url}");

    tokio::signal::ctrl_c().await?;
    println!("[server] shutting down");
    Ok(())
}
