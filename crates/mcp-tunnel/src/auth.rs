use serde::Deserialize;

pub struct AuthProviderConfig {
    pub url: String,
    pub secret: String,
    pub client: reqwest::Client,
}

#[derive(Deserialize)]
struct AuthVerifyResponse {
    subdomains: Vec<String>,
}

#[derive(Deserialize)]
struct AuthErrorResponse {
    error: String,
}

#[derive(Debug)]
pub enum AuthError {
    InvalidToken(String),
    ProviderUnavailable(String),
}

pub async fn verify_token(
    auth: &AuthProviderConfig,
    token: &str,
    subdomain: &str,
) -> Result<Vec<String>, AuthError> {
    let resp = auth
        .client
        .post(format!("{}/api/verify", auth.url))
        .header("X-Relay-Secret", &auth.secret)
        .json(&serde_json::json!({
            "token": token,
            "subdomain": subdomain,
        }))
        .send()
        .await
        .map_err(|e| AuthError::ProviderUnavailable(e.to_string()))?;

    match resp.status().as_u16() {
        200 => {
            let body: AuthVerifyResponse = resp
                .json()
                .await
                .map_err(|e| AuthError::ProviderUnavailable(format!("bad response: {e}")))?;
            Ok(body.subdomains)
        }
        401 | 403 => {
            let msg = resp
                .json::<AuthErrorResponse>()
                .await
                .map(|r| r.error)
                .unwrap_or_else(|_| "invalid token".into());
            Err(AuthError::InvalidToken(msg))
        }
        status => Err(AuthError::ProviderUnavailable(format!(
            "unexpected status {status}"
        ))),
    }
}

pub fn subdomain_matches(patterns: &[String], subdomain: &str) -> bool {
    patterns.iter().any(|p| glob_match(p, subdomain))
}

fn glob_match(pattern: &str, value: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == value,
        Some((prefix, suffix)) => {
            value.starts_with(prefix)
                && value[prefix.len()..].ends_with(suffix)
                && value.len() >= prefix.len() + suffix.len()
        }
    }
}
