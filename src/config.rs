use std::collections::HashMap;

pub struct RelayConfig {
    pub port: u16,
    pub relay_domain: String,
    pub auth_provider: Option<String>,
    pub auth_provider_secret: Option<String>,
    /// Static token to allowed subdomains map. Parsed from MCPR_RELAY_TOKENS JSON.
    pub tokens: HashMap<String, Vec<String>>,
    pub max_request_body_size: Option<usize>,
    pub max_response_body_size: Option<usize>,
}

pub fn load_from_env() -> RelayConfig {
    let port = std::env::var("MCPR_RELAY_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(8080);

    let relay_domain = std::env::var("MCPR_RELAY_DOMAIN").unwrap_or_else(|_| {
        eprintln!("error: MCPR_RELAY_DOMAIN is required");
        std::process::exit(1);
    });

    let auth_mode = std::env::var("MCPR_RELAY_AUTH_MODE").unwrap_or_else(|_| "open".to_string());

    let (auth_provider, auth_provider_secret) = match auth_mode.as_str() {
        "provider" => {
            let url = std::env::var("MCPR_RELAY_AUTH_URL").unwrap_or_else(|_| {
                eprintln!(
                    "error: MCPR_RELAY_AUTH_URL is required when MCPR_RELAY_AUTH_MODE=provider"
                );
                std::process::exit(1);
            });
            let secret = std::env::var("MCPR_RELAY_AUTH_SECRET").unwrap_or_else(|_| {
                eprintln!(
                    "error: MCPR_RELAY_AUTH_SECRET is required when MCPR_RELAY_AUTH_MODE=provider"
                );
                std::process::exit(1);
            });
            (Some(url), Some(secret))
        }
        "open" | "static" => (None, None),
        other => {
            eprintln!(
                "error: unknown MCPR_RELAY_AUTH_MODE: {other} (expected: open, static, provider)"
            );
            std::process::exit(1);
        }
    };

    // MCPR_RELAY_TOKENS accepts a JSON array: [{"token":"tok_abc","subdomains":["myapp","myapp-*"]}]
    let tokens = parse_tokens_env();

    let max_request_body_size = std::env::var("MCPR_RELAY_MAX_REQUEST_BODY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok());

    let max_response_body_size = std::env::var("MCPR_RELAY_MAX_RESPONSE_BODY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok());

    RelayConfig {
        port,
        relay_domain,
        auth_provider,
        auth_provider_secret,
        tokens,
        max_request_body_size,
        max_response_body_size,
    }
}

#[derive(serde::Deserialize)]
struct TokenEntry {
    token: String,
    subdomains: Vec<String>,
}

fn parse_tokens_env() -> HashMap<String, Vec<String>> {
    let raw = match std::env::var("MCPR_RELAY_TOKENS") {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };

    match serde_json::from_str::<Vec<TokenEntry>>(&raw) {
        Ok(entries) => entries
            .into_iter()
            .map(|e| (e.token, e.subdomains))
            .collect(),
        Err(e) => {
            eprintln!("error: failed to parse MCPR_RELAY_TOKENS: {e}");
            eprintln!(r#"  expected: [{{"token":"tok_abc","subdomains":["myapp","myapp-*"]}}]"#);
            std::process::exit(1);
        }
    }
}
