//! Client library for connecting a local HTTP service to an [`mcp-tunnel`](https://github.com/pragmalabs-tech/mcp-tunnel) relay.
//!
//! ```no_run
//! use mcp_tunnel_client::{TunnelStatusCallback, start_tunnel_client};
//!
//! struct MyStatus;
//! impl TunnelStatusCallback for MyStatus {
//!     fn on_connected(&self, url: &str) { println!("public URL: {url}"); }
//!     fn on_disconnected(&self) {}
//!     fn on_evicted(&self) {}
//! }
//!
//! # async fn run() -> Result<(), String> {
//! let public_url = start_tunnel_client(
//!     9000,
//!     "https://tunnel.example.com",
//!     "tok_abc",
//!     Some("myapp"),
//!     MyStatus,
//! ).await?;
//! # Ok(()) }
//! ```

mod client;
pub mod protocol;

pub use client::{TunnelStatusCallback, start_tunnel_client};
