use std::collections::HashMap;

use clap::Parser;

#[derive(Parser)]
#[command(version, about = "Self-hosted relay server for mcpr tunneling")]
pub struct Cli {
    /// Base domain for tunnel subdomains (e.g. tunnel.example.com)
    #[arg(long)]
    pub domain: String,

    /// TCP listen port
    #[arg(long, default_value_t = 8080)]
    pub port: u16,

    /// Static token, format: TOKEN:SUBDOMAIN[,SUBDOMAIN...] (repeatable)
    #[arg(long = "static-token", value_name = "TOKEN:SUBS")]
    pub static_tokens: Vec<String>,

    /// Auth provider URL (enables provider auth mode)
    #[arg(long, requires = "auth_secret")]
    pub auth_url: Option<String>,

    /// Shared secret sent as X-Relay-Secret header to the auth provider
    #[arg(long, requires = "auth_url")]
    pub auth_secret: Option<String>,

    /// Max inbound request body in bytes
    #[arg(long, default_value_t = 5 * 1024 * 1024)]
    pub max_request_body: usize,

    /// Max tunneled response body in bytes
    #[arg(long, default_value_t = 10 * 1024 * 1024)]
    pub max_response_body: usize,
}

pub struct RelayConfig {
    pub port: u16,
    pub relay_domain: String,
    pub auth_provider: Option<String>,
    pub auth_provider_secret: Option<String>,
    pub tokens: HashMap<String, Vec<String>>,
    pub max_request_body_size: Option<usize>,
    pub max_response_body_size: Option<usize>,
}

pub fn load() -> RelayConfig {
    let cli = Cli::parse();

    if !cli.static_tokens.is_empty() && cli.auth_url.is_some() {
        eprintln!("error: --static-token and --auth-url are mutually exclusive");
        std::process::exit(1);
    }

    RelayConfig {
        port: cli.port,
        relay_domain: cli.domain,
        auth_provider: cli.auth_url,
        auth_provider_secret: cli.auth_secret,
        tokens: parse_tokens(&cli.static_tokens),
        max_request_body_size: Some(cli.max_request_body),
        max_response_body_size: Some(cli.max_response_body),
    }
}

fn parse_tokens(entries: &[String]) -> HashMap<String, Vec<String>> {
    let mut map = HashMap::new();
    for entry in entries {
        let Some((token, subs)) = entry.split_once(':') else {
            eprintln!("error: invalid --static-token format: {entry}");
            eprintln!("  expected: TOKEN:SUBDOMAIN[,SUBDOMAIN...]");
            std::process::exit(1);
        };
        let subs: Vec<String> = subs
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if subs.is_empty() {
            eprintln!("error: --static-token {token:?} has no subdomains");
            std::process::exit(1);
        }
        map.insert(token.to_string(), subs);
    }
    map
}
