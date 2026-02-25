use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use flate2::read::GzDecoder;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, ACCEPT_ENCODING, AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

use crate::providers::OAuthProvider;

pub mod streamer;

pub const SCHWAB_AUTH_URL: &str = "https://api.schwabapi.com/v1/oauth/authorize";
pub const SCHWAB_TOKEN_URL: &str = "https://api.schwabapi.com/v1/oauth/token";

#[derive(Debug, Clone)]
pub struct SchwabOAuthConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub scope: String,
}

impl SchwabOAuthConfig {
    pub fn from_env() -> Result<Self, String> {
        let client_id = env::var("SCHWAB_CLIENT_ID")
            .map_err(|_| "missing SCHWAB_CLIENT_ID environment variable".to_string())?;
        let client_secret = env::var("SCHWAB_CLIENT_SECRET").ok();
        let redirect_uri = env::var("SCHWAB_REDIRECT_URI")
            .map_err(|_| "missing SCHWAB_REDIRECT_URI environment variable".to_string())?;
        let scope = env::var("SCHWAB_SCOPE").unwrap_or_else(|_| "readonly".to_string());

        let cfg = Self {
            client_id,
            client_secret,
            redirect_uri,
            scope,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.client_id.trim().is_empty() {
            return Err("SCHWAB_CLIENT_ID cannot be empty".to_string());
        }

        if !self.redirect_uri.starts_with("https://") {
            return Err("SCHWAB_REDIRECT_URI must use https".to_string());
        }
        Ok(())
    }
}

pub struct SchwabOAuth {
    cfg: SchwabOAuthConfig,
    client: Client,
}

impl SchwabOAuth {
    pub fn new(cfg: SchwabOAuthConfig) -> Self {
        Self {
            cfg,
            client: Client::new(),
        }
    }

    pub fn exchange_authorization_code(&self, code: &str) -> Result<TokenResponse, String> {
        let mut form = HashMap::<&str, String>::new();
        form.insert("grant_type", "authorization_code".to_string());
        form.insert("code", code.to_string());
        form.insert("redirect_uri", self.cfg.redirect_uri.clone());
        form.insert("client_id", self.cfg.client_id.clone());
        self.request_token(form)
    }

    pub fn refresh_access_token(&self, refresh_token: &str) -> Result<TokenResponse, String> {
        let mut form = HashMap::<&str, String>::new();
        form.insert("grant_type", "refresh_token".to_string());
        form.insert("refresh_token", refresh_token.to_string());
        form.insert("client_id", self.cfg.client_id.clone());
        self.request_token(form)
    }

    fn request_token(&self, form: HashMap<&str, String>) -> Result<TokenResponse, String> {
        self.cfg.validate()?;

        let mut request = self
            .client
            .post(SCHWAB_TOKEN_URL)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(ACCEPT, "application/json")
            .header(ACCEPT_ENCODING, "identity")
            .form(&form);

        if let Some(secret) = self.cfg.client_secret.as_ref() {
            let token = STANDARD.encode(format!("{}:{}", self.cfg.client_id, secret));
            request = request.header(AUTHORIZATION, format!("Basic {}", token));
        }

        let response = request.send().map_err(|e| e.to_string())?;
        let status = response.status();
        let encoding = response
            .headers()
            .get(CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_ascii_lowercase());
        let body_bytes = response.bytes().map_err(|e| e.to_string())?;
        let body = decode_http_body(&body_bytes, encoding.as_deref());
        if !status.is_success() {
            return Err(format!(
                "token request failed with status {}: {}",
                status, body
            ));
        }

        serde_json::from_str(&body).map_err(|e| format!("failed to parse token response: {}", e))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: Option<String>,
    pub expires_in: Option<u64>,
    pub scope: Option<String>,
    pub refresh_token: Option<String>,
    pub refresh_token_expires_in: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StoredTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: Option<String>,
    pub scope: Option<String>,
    pub expires_in: Option<u64>,
    pub refresh_token_expires_in: Option<u64>,
    pub obtained_at_epoch_secs: u64,
}

impl StoredTokens {
    pub fn from_response(resp: TokenResponse) -> Self {
        Self {
            access_token: resp.access_token,
            refresh_token: resp.refresh_token,
            token_type: resp.token_type,
            scope: resp.scope,
            expires_in: resp.expires_in,
            refresh_token_expires_in: resp.refresh_token_expires_in,
            obtained_at_epoch_secs: now_epoch_secs(),
        }
    }
}

pub fn default_token_file_path() -> PathBuf {
    if let Ok(path) = env::var("CHART_TUI_SCHWAB_TOKEN_FILE") {
        return PathBuf::from(path);
    }

    if let Ok(cfg_home) = env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(cfg_home)
            .join("chart_tui")
            .join("schwab_tokens.json");
    }

    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("chart_tui")
            .join("schwab_tokens.json");
    }

    PathBuf::from("schwab_tokens.json")
}

pub fn save_tokens(path: &Path, tokens: &StoredTokens) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(tokens).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}

pub fn load_tokens(path: &Path) -> Result<StoredTokens, String> {
    let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&raw).map_err(|e| e.to_string())
}

pub fn extract_auth_code_and_state(input: &str) -> Result<(String, Option<String>), String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("authorization callback input is empty".to_string());
    }

    if !trimmed.contains("://") && !trimmed.contains('?') {
        return Ok((trimmed.to_string(), None));
    }

    let query = trimmed
        .split_once('?')
        .map(|(_, q)| q)
        .ok_or_else(|| "callback URL is missing query string".to_string())?;

    let query = query.split('#').next().unwrap_or(query);
    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    for pair in query.split('&') {
        let (k, v) = match pair.split_once('=') {
            Some(parts) => parts,
            None => continue,
        };
        let decoded = percent_decode(v)?;
        match k {
            "code" => code = Some(decoded),
            "state" => state = Some(decoded),
            _ => {}
        }
    }

    let code = code.ok_or_else(|| "callback URL missing code parameter".to_string())?;
    Ok((code, state))
}

impl OAuthProvider for SchwabOAuth {
    fn authorization_url(&self, state: &str) -> Result<String, String> {
        self.cfg.validate()?;
        let qs = format!(
            "response_type=code&client_id={}&scope={}&redirect_uri={}&state={}",
            percent_encode(&self.cfg.client_id),
            percent_encode(&self.cfg.scope),
            percent_encode(&self.cfg.redirect_uri),
            percent_encode(state),
        );
        Ok(format!("{SCHWAB_AUTH_URL}?{qs}"))
    }

    fn token_url(&self) -> &'static str {
        SCHWAB_TOKEN_URL
    }
}

fn percent_encode(input: &str) -> String {
    let mut out = String::new();
    for &b in input.as_bytes() {
        let keep = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if keep {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push_str(&format!("{:02X}", b));
        }
    }
    out
}

fn percent_decode(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                if i + 2 >= bytes.len() {
                    return Err("invalid percent-encoding in callback URL".to_string());
                }
                let hi = hex_value(bytes[i + 1])?;
                let lo = hex_value(bytes[i + 2])?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| "callback URL contained invalid UTF-8".to_string())
}

fn hex_value(b: u8) -> Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(10 + (b - b'a')),
        b'A'..=b'F' => Ok(10 + (b - b'A')),
        _ => Err("invalid percent-encoding in callback URL".to_string()),
    }
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn decode_http_body(body: &[u8], encoding: Option<&str>) -> String {
    let is_gzip = encoding
        .map(|e| e.contains("gzip"))
        .unwrap_or_else(|| body.starts_with(&[0x1f, 0x8b]));
    if is_gzip {
        let mut decoder = GzDecoder::new(body);
        let mut decoded = String::new();
        if decoder.read_to_string(&mut decoded).is_ok() {
            return decoded;
        }
    }
    String::from_utf8_lossy(body).to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        default_token_file_path, extract_auth_code_and_state, load_tokens, save_tokens,
        SchwabOAuth, SchwabOAuthConfig, StoredTokens,
    };
    use crate::providers::OAuthProvider;
    use std::path::PathBuf;

    #[test]
    fn builds_authorization_url_with_required_params() {
        let cfg = SchwabOAuthConfig {
            client_id: "cid123".to_string(),
            client_secret: None,
            redirect_uri: "https://127.0.0.1".to_string(),
            scope: "readonly".to_string(),
        };
        let oauth = SchwabOAuth::new(cfg);
        let url = oauth.authorization_url("state-abc").expect("auth url");

        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=cid123"));
        assert!(url.contains("scope=readonly"));
        assert!(url.contains("redirect_uri=https%3A%2F%2F127.0.0.1"));
        assert!(url.contains("state=state-abc"));
    }

    #[test]
    fn extracts_code_and_state_from_callback_url() {
        let input = "https://127.0.0.1/callback?code=abc%20123&state=s1";
        let (code, state) = extract_auth_code_and_state(input).expect("parse callback");
        assert_eq!(code, "abc 123");
        assert_eq!(state.as_deref(), Some("s1"));
    }

    #[test]
    fn allows_pasting_code_directly() {
        let (code, state) = extract_auth_code_and_state("my-code-value").expect("parse code");
        assert_eq!(code, "my-code-value");
        assert_eq!(state, None);
    }

    #[test]
    fn token_round_trip_file_io_works() {
        let mut path = PathBuf::from(std::env::temp_dir());
        path.push(format!(
            "chart_tui_schwab_token_test_{}_{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("t")
        ));

        let tokens = StoredTokens {
            access_token: "access".to_string(),
            refresh_token: Some("refresh".to_string()),
            token_type: Some("Bearer".to_string()),
            scope: Some("readonly".to_string()),
            expires_in: Some(1800),
            refresh_token_expires_in: Some(86400),
            obtained_at_epoch_secs: 123,
        };

        save_tokens(&path, &tokens).expect("save tokens");
        let loaded = load_tokens(&path).expect("load tokens");
        assert_eq!(loaded.access_token, "access");
        assert_eq!(loaded.refresh_token.as_deref(), Some("refresh"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn default_token_path_has_expected_filename() {
        let path = default_token_file_path();
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("schwab_tokens.json")
        );
    }
}
