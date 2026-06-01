use url::form_urlencoded::Serializer;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::types::{PkceCodes, ProviderId, RefreshTokenExhaustedError, TokenData};
use crate::utils::{decode_jwt_payload, expires_in_iso};

pub const ANTHROPIC_AUTH_URL: &str = "https://claude.ai/oauth/authorize";
pub const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const ANTHROPIC_REDIRECT_URI: &str = "http://localhost:54545/callback";
pub const ANTHROPIC_SCOPE: &str = "org:create_api_key user:profile user:inference";

pub const CODEX_ISSUER: &str = "https://auth.openai.com";
pub const CODEX_AUTH_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const CODEX_CALLBACK_PORT: u16 = 1455;
pub const CODEX_CALLBACK_PATH: &str = "/auth/callback";
pub const CODEX_SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
pub const CODEX_ORIGINATOR: &str = "codex_cli_rs";
pub const ANTHROPIC_TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
pub const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

#[must_use]
pub fn detect_exhausted_reason(body: &str) -> Option<&'static str> {
    let body = body.to_ascii_lowercase();
    [
        "refresh_token_reused",
        "invalid_grant",
        "expired",
        "invalidated",
        "revoked",
    ]
    .into_iter()
    .find(|marker| body.contains(marker))
}

#[must_use]
pub fn generate_anthropic_auth_url(state: &str, pkce: &PkceCodes) -> String {
    let query = Serializer::new(String::new())
        .append_pair("code", "true")
        .append_pair("client_id", ANTHROPIC_CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", ANTHROPIC_REDIRECT_URI)
        .append_pair("code_challenge", &pkce.code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("scope", ANTHROPIC_SCOPE)
        .finish();
    format!("{ANTHROPIC_AUTH_URL}?{query}")
}

#[must_use]
pub fn generate_codex_auth_url(state: &str, pkce: &PkceCodes) -> String {
    let redirect_uri = format!("http://localhost:{CODEX_CALLBACK_PORT}{CODEX_CALLBACK_PATH}");
    let query = Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", CODEX_CLIENT_ID)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", CODEX_SCOPE)
        .append_pair("code_challenge", &pkce.code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("state", state)
        .append_pair("originator", CODEX_ORIGINATOR)
        .finish();
    format!("{CODEX_AUTH_URL}?{query}")
}

/// Exchange an Anthropic OAuth authorization code for a stored token.
///
/// # Errors
///
/// Returns an error when OAuth state does not match, the token endpoint fails, or the response
/// body does not contain the expected token fields.
pub async fn exchange_anthropic_code(
    code: &str,
    returned_state: &str,
    expected_state: &str,
    pkce: &PkceCodes,
) -> Result<TokenData> {
    ensure_state(returned_state, expected_state)?;
    let response = reqwest::Client::new()
        .post(ANTHROPIC_TOKEN_URL)
        .json(&serde_json::json!({
            "code": code,
            "grant_type": "authorization_code",
            "client_id": ANTHROPIC_CLIENT_ID,
            "redirect_uri": ANTHROPIC_REDIRECT_URI,
            "code_verifier": pkce.code_verifier,
            "state": expected_state
        }))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        bail!("token exchange failed ({status}): {body}");
    }
    anthropic_token(&serde_json::from_str(&body).context("Anthropic token response is not JSON")?)
}

/// Build the Anthropic OAuth refresh request (JSON body).
fn anthropic_refresh_request(refresh_token: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .post(ANTHROPIC_TOKEN_URL)
        .json(&serde_json::json!({
            "client_id": ANTHROPIC_CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        }))
        .timeout(std::time::Duration::from_secs(30))
}

/// Refresh an Anthropic OAuth token.
///
/// # Errors
///
/// Returns an error when the token endpoint fails or the response body is invalid.
pub async fn refresh_anthropic_tokens(refresh_token: String) -> Result<TokenData> {
    let response = anthropic_refresh_request(&refresh_token).send().await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        if let Some(reason) = detect_exhausted_reason(&body) {
            return Err(
                RefreshTokenExhaustedError::new(reason, Some(status.as_u16()), Some(body)).into(),
            );
        }
        bail!("token refresh failed ({status}): {body}");
    }
    anthropic_token(&serde_json::from_str(&body).context("Anthropic refresh response is not JSON")?)
}

/// Exchange a Codex OAuth authorization code for a stored token.
///
/// # Errors
///
/// Returns an error when OAuth state does not match, the token endpoint fails, or the response
/// body does not contain the expected token fields.
pub async fn exchange_codex_code(
    code: &str,
    returned_state: &str,
    expected_state: &str,
    pkce: &PkceCodes,
) -> Result<TokenData> {
    ensure_state(returned_state, expected_state)?;
    let redirect_uri = format!("http://localhost:{CODEX_CALLBACK_PORT}{CODEX_CALLBACK_PATH}");
    let response = reqwest::Client::new()
        .post(CODEX_TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", CODEX_CLIENT_ID),
            ("code_verifier", pkce.code_verifier.as_str()),
        ])
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        bail!("codex token exchange failed ({status}): {body}");
    }
    codex_token(&serde_json::from_str(&body).context("Codex token response is not JSON")?)
}

/// Build the Codex OAuth refresh request (form-encoded body).
fn codex_refresh_request(refresh_token: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .post(CODEX_TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", CODEX_CLIENT_ID),
            ("refresh_token", refresh_token),
        ])
        .timeout(std::time::Duration::from_secs(30))
}

/// Refresh a Codex OAuth token.
///
/// # Errors
///
/// Returns an error when the token endpoint fails or the response body is invalid.
pub async fn refresh_codex_tokens(refresh_token: String) -> Result<TokenData> {
    let response = codex_refresh_request(&refresh_token).send().await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        if let Some(reason) = detect_exhausted_reason(&body) {
            return Err(
                RefreshTokenExhaustedError::new(reason, Some(status.as_u16()), Some(body)).into(),
            );
        }
        bail!("codex token refresh failed ({status}): {body}");
    }
    codex_token(&serde_json::from_str(&body).context("Codex refresh response is not JSON")?)
}

fn ensure_state(returned_state: &str, expected_state: &str) -> Result<()> {
    if returned_state != expected_state {
        bail!("OAuth state mismatch");
    }
    Ok(())
}

fn anthropic_token(data: &Value) -> Result<TokenData> {
    let account = data.get("account").unwrap_or(&Value::Null);
    Ok(TokenData {
        access_token: required_string(data, "access_token")?,
        refresh_token: required_string(data, "refresh_token")?,
        email: account
            .get("email_address")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        expires_at: expires_in_iso(data.get("expires_in").and_then(Value::as_u64), 3600),
        account_uuid: account
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        provider: ProviderId::Anthropic,
        id_token: None,
        last_refresh_at: None,
        plan_type: None,
        cursor: None,
    })
}

fn codex_token(data: &Value) -> Result<TokenData> {
    let id_token = required_string(data, "id_token")?;
    let claims = decode_jwt_payload(&id_token)?;
    let auth = claims
        .get("https://api.openai.com/auth")
        .unwrap_or(&Value::Null);
    let email = claims
        .get("email")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let account_uuid = auth
        .get("chatgpt_account_id")
        .or_else(|| claims.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let plan_type = auth
        .get("chatgpt_plan_type")
        .or_else(|| claims.get("chatgpt_plan_type"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    Ok(TokenData {
        access_token: required_string(data, "access_token")?,
        refresh_token: required_string(data, "refresh_token")?,
        email,
        expires_at: expires_in_iso(data.get("expires_in").and_then(Value::as_u64), 3600),
        account_uuid,
        provider: ProviderId::Codex,
        id_token: Some(id_token),
        last_refresh_at: None,
        plan_type,
        cursor: None,
    })
}

fn required_string(data: &Value, field: &str) -> Result<String> {
    data.get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .with_context(|| format!("token response is missing {field}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content_type(req: &reqwest::Request) -> String {
        req.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    }

    fn body_string(req: &reqwest::Request) -> String {
        let bytes = req
            .body()
            .and_then(reqwest::Body::as_bytes)
            .expect("refresh request always sets a body");
        std::str::from_utf8(bytes)
            .expect("refresh request body is valid utf-8")
            .to_owned()
    }

    #[test]
    fn codex_refresh_uses_form_encoded_request() {
        let req = codex_refresh_request("refresh-xyz")
            .build()
            .expect("codex refresh request builds");
        assert_eq!(req.url().as_str(), CODEX_TOKEN_URL);
        assert_eq!(content_type(&req), "application/x-www-form-urlencoded");
        let body = body_string(&req);
        assert!(body.contains("grant_type=refresh_token"), "{body}");
        assert!(body.contains("refresh_token=refresh-xyz"), "{body}");
    }

    #[test]
    fn anthropic_refresh_uses_json_request() {
        let req = anthropic_refresh_request("refresh-xyz")
            .build()
            .expect("anthropic refresh request builds");
        assert_eq!(req.url().as_str(), ANTHROPIC_TOKEN_URL);
        assert_eq!(content_type(&req), "application/json");
        let body = body_string(&req);
        assert!(body.contains("\"grant_type\":\"refresh_token\""), "{body}");
        assert!(body.contains("\"refresh_token\":\"refresh-xyz\""), "{body}");
    }
}
