//! mcp-tunnel relay library.
//!
//! The binary at `src/main.rs` is a thin shell around [`relay::build_relay_app`].
//! Tests and embedders pull in the same library entry points here.

pub mod auth;
pub mod config;
pub mod relay;
