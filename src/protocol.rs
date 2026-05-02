use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

#[derive(Serialize, Deserialize)]
pub struct TunnelRequest {
    pub id: String,
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Option<String>, // base64
}

#[derive(Serialize, Deserialize)]
pub struct TunnelResponse {
    pub id: String,
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Option<String>, // base64
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

#[derive(Serialize, Deserialize)]
pub struct SubdomainOffer {
    pub subdomains: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct SubdomainPick {
    pub subdomain: String,
}
