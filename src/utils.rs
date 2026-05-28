use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine;
use chrono::{SecondsFormat, Utc};
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::types::PkceCodes;

#[must_use]
pub fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[must_use]
pub fn expires_in_iso(seconds: Option<u64>, default_seconds: u64) -> String {
    let ttl = i64::try_from(seconds.unwrap_or(default_seconds)).unwrap_or(i64::MAX);
    (Utc::now() + chrono::Duration::seconds(ttl)).to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[must_use]
pub fn resolve_auth_dir(path: &str, home: &Path) -> PathBuf {
    if path == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(path)
}

#[must_use]
pub fn generate_api_key() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    format!(
        "sk-local-{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

#[must_use]
pub fn generate_pkce_codes() -> PkceCodes {
    let mut bytes = [0_u8; 96];
    rand::rng().fill_bytes(&mut bytes);
    let code_verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(code_verifier.as_bytes()));
    PkceCodes {
        code_verifier,
        code_challenge,
    }
}

#[must_use]
pub fn random_urlsafe(bytes_len: usize) -> String {
    let mut bytes = vec![0_u8; bytes_len];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[must_use]
pub fn sanitize_email(email: &str) -> String {
    email.replace('/', "_")
}

/// Decode the JSON payload segment of a JWT without verifying the signature.
///
/// # Errors
///
/// Returns an error when the token does not contain a payload segment, the segment is not
/// base64url, or the decoded bytes are not JSON.
pub fn decode_jwt_payload(token: &str) -> Result<Value> {
    let payload = token
        .split('.')
        .nth(1)
        .context("JWT is missing payload segment")?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .context("JWT payload is not base64url")?;
    serde_json::from_slice(&decoded).context("JWT payload is not JSON")
}

#[must_use]
pub fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
