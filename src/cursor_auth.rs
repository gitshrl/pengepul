use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::types::{CursorMeta, ProviderId, RefreshTokenExhaustedError, TokenData};
use crate::utils::decode_jwt_payload;

pub const CURSOR_CLIENT_ID: &str = "KbZUR41cY7W6zRSdpSUJ7I7mLYBKOCmB";
pub const CURSOR_DEFAULT_CLIENT_VERSION: &str = "cli-2026.01.09-231024f";
pub const CURSOR_TOKEN_URL: &str = "https://api2.cursor.sh/oauth/token";

fn cursor_refresh_request(refresh_token: &str, client_id: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .post(CURSOR_TOKEN_URL)
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": client_id,
            "refresh_token": refresh_token,
        }))
        .timeout(std::time::Duration::from_secs(30))
}

fn expiry_from_jwt(access_token: &str) -> String {
    if let Ok(claims) = decode_jwt_payload(access_token)
        && let Some(exp) = claims.get("exp").and_then(Value::as_i64)
        && let Some(dt) = chrono::DateTime::from_timestamp(exp, 0)
    {
        return dt.to_rfc3339();
    }
    (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()
}

/// Refresh a Cursor access token. The refresh credential must be a real refresh token
/// (never the access token), or the request is refused before hitting the network.
///
/// # Errors
/// Returns `RefreshTokenExhaustedError` on an invalidated refresh token, or a generic error on
/// transport/HTTP failure.
pub async fn refresh_cursor_tokens(refresh_token: String) -> Result<TokenData> {
    if refresh_token.trim().is_empty() {
        return Err(RefreshTokenExhaustedError::new(
            "invalidated",
            None,
            Some("no refresh token stored; re-run login --provider cursor".into()),
        )
        .into());
    }
    let response = cursor_refresh_request(&refresh_token, CURSOR_CLIENT_ID)
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        if let Some(reason) = crate::oauth::detect_exhausted_reason(&body) {
            return Err(
                RefreshTokenExhaustedError::new(reason, Some(status.as_u16()), Some(body)).into(),
            );
        }
        bail!("cursor token refresh failed ({status}): {body}");
    }
    let data: Value = serde_json::from_str(&body).context("Cursor refresh response is not JSON")?;
    if data
        .get("shouldLogout")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(
            RefreshTokenExhaustedError::new("invalidated", Some(status.as_u16()), Some(body)).into(),
        );
    }
    let access_token = data
        .get("access_token")
        .and_then(Value::as_str)
        .context("cursor refresh response missing access_token")?
        .to_string();
    let new_refresh = data
        .get("refresh_token")
        .and_then(Value::as_str)
        .map_or(refresh_token, ToOwned::to_owned);
    Ok(TokenData {
        access_token: access_token.clone(),
        refresh_token: new_refresh,
        email: String::new(), // preserved by AccountManager from the old token
        expires_at: expiry_from_jwt(&access_token),
        account_uuid: String::new(), // preserved by AccountManager
        provider: ProviderId::Cursor,
        id_token: data
            .get("id_token")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        last_refresh_at: None,
        plan_type: None,
        cursor: Some(CursorMeta {
            service_machine_id: None,
            client_version: CURSOR_DEFAULT_CLIENT_VERSION.to_string(),
            config_version: String::new(),
            client_id: CURSOR_CLIENT_ID.to_string(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_request_is_json_with_refresh_grant() {
        let req = cursor_refresh_request("rt-123", CURSOR_CLIENT_ID)
            .build()
            .expect("builds");
        assert_eq!(req.url().as_str(), CURSOR_TOKEN_URL);
        let ct = req
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(ct, "application/json");
        let body = std::str::from_utf8(req.body().unwrap().as_bytes().unwrap()).unwrap();
        assert!(body.contains("\"grant_type\":\"refresh_token\""), "{body}");
        assert!(body.contains("\"refresh_token\":\"rt-123\""), "{body}");
    }
}
