//! HTTP Basic Auth middleware.
//!
//! Credentials come from env vars (populated by install_global.sh):
//!   WEB_USER  = "alice"
//!   WEB_AUTH  = "sha256:<salt_hex>:<hash_hex>"
//!
//! where `hash = sha256(salt || password)`. Colon-separated to avoid docker
//! compose variable interpolation (which treats `$` specially).
//! If either is missing, the UI runs anonymously — logged as WARNING.

use axum::{
    body::Body,
    extract::State,
    http::{header, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::prelude::*;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::AppState;
use crate::config::Config;

pub struct AuthConfig {
    pub username: Option<String>,
    pub salt:     Vec<u8>,
    pub hash:     Vec<u8>,
}

impl AuthConfig {
    pub fn load(cfg: &Config) -> Self {
        let username = cfg.web_user.clone();
        let parsed = cfg.web_auth.as_deref().and_then(parse_auth);
        let (salt, hash) = parsed.unwrap_or_default();
        Self { username, salt, hash }
    }
    pub fn is_anonymous(&self) -> bool {
        self.username.is_none() || self.salt.is_empty() || self.hash.is_empty()
    }
    pub fn verify(&self, user: &str, pass: &str) -> bool {
        let Some(u) = self.username.as_deref() else { return false };
        if self.salt.is_empty() || self.hash.is_empty() { return false; }
        // Constant-time username compare (length-aware).
        if u.len() != user.len() { return false; }
        if u.as_bytes().ct_eq(user.as_bytes()).unwrap_u8() == 0 { return false; }

        let mut h = Sha256::new();
        h.update(&self.salt);
        h.update(pass.as_bytes());
        let computed = h.finalize();
        computed[..].ct_eq(&self.hash).unwrap_u8() == 1
    }
}

fn parse_auth(s: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    // "sha256:<salt_hex>:<hash_hex>"
    let mut parts = s.split(':');
    if parts.next() != Some("sha256") { return None; }
    let salt = hex::decode(parts.next()?).ok()?;
    let hash = hex::decode(parts.next()?).ok()?;
    Some((salt, hash))
}

pub async fn basic_auth(
    State(st): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    // If nothing is configured we let everything through (for --no-auth scenarios);
    // `serve` already logs a WARNING about it.
    if st.auth.is_anonymous() {
        return next.run(req).await;
    }

    let creds = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Basic "))
        .and_then(|b64| BASE64_STANDARD.decode(b64).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok());

    if let Some(s) = creds {
        if let Some((user, pass)) = s.split_once(':') {
            if st.auth.verify(user, pass) {
                return next.run(req).await;
            }
        }
    }
    unauthorized()
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [
            (header::WWW_AUTHENTICATE, "Basic realm=\"codeingraph2\", charset=\"UTF-8\""),
            (header::CONTENT_TYPE,     "text/plain; charset=utf-8"),
        ],
        "401 Unauthorized — provide credentials in the browser prompt.\n",
    ).into_response()
}
