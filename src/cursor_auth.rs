use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

const CURSOR_KEYS: [&str; 6] = [
    "cursorAuth/accessToken",
    "cursorAuth/refreshToken",
    "cursorAuth/cachedEmail",
    "cursorAuth/clientVersion",
    "cursorAuth/configVersion",
    "storage.serviceMachineId",
];

#[must_use]
pub fn default_cursor_storage_path(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb")
    } else if cfg!(target_os = "windows") {
        home.join("AppData/Roaming/Cursor/User/globalStorage/state.vscdb")
    } else {
        home.join(".config/Cursor/User/globalStorage/state.vscdb")
    }
}

fn read_cursor_sqlite(path: &Path) -> Result<BTreeMap<String, String>> {
    let conn = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("failed to open cursor storage {}", path.display()))?;
    let mut out = BTreeMap::new();
    for table in ["ItemTable", "cursorDiskKV"] {
        let sql = format!(
            "SELECT key, value FROM {table} WHERE key IN ({})",
            CURSOR_KEYS.iter().map(|_| "?").collect::<Vec<_>>().join(",")
        );
        let Ok(mut stmt) = conn.prepare(&sql) else {
            continue;
        };
        let rows = stmt.query_map(rusqlite::params_from_iter(CURSOR_KEYS.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        });
        if let Ok(rows) = rows {
            for row in rows.flatten() {
                out.insert(row.0, row.1);
            }
        }
    }
    Ok(out)
}

#[must_use]
fn coerce(value: &str) -> String {
    // Cursor stores some values JSON-wrapped (`"foo"`); unwrap a JSON string if present.
    serde_json::from_str::<String>(value).unwrap_or_else(|_| value.to_string())
}

/// Build a Cursor `TokenData` from a storage key/value map.
///
/// # Errors
/// Returns an error when access or refresh tokens are absent.
pub fn cursor_token_from_storage(storage: &BTreeMap<String, String>) -> Result<TokenData> {
    let access_token = storage
        .get("cursorAuth/accessToken")
        .map(|v| coerce(v))
        .filter(|v| !v.is_empty())
        .context("cursor storage missing accessToken")?;
    let refresh_token = storage
        .get("cursorAuth/refreshToken")
        .map(|v| coerce(v))
        .filter(|v| !v.is_empty())
        .context("cursor storage missing refreshToken")?;
    let machine_id = storage.get("storage.serviceMachineId").map(|v| coerce(v));
    Ok(TokenData {
        access_token: access_token.clone(),
        refresh_token,
        email: storage
            .get("cursorAuth/cachedEmail")
            .map(|v| coerce(v))
            .unwrap_or_else(|| "unknown".into()),
        expires_at: expiry_from_jwt(&access_token),
        account_uuid: machine_id.clone().unwrap_or_default(),
        provider: ProviderId::Cursor,
        id_token: None,
        last_refresh_at: None,
        plan_type: storage.get("cursorAuth/stripeMembershipType").map(|v| coerce(v)),
        cursor: Some(CursorMeta {
            service_machine_id: machine_id,
            client_version: storage
                .get("cursorAuth/clientVersion")
                .map(|v| coerce(v))
                .unwrap_or_else(|| CURSOR_DEFAULT_CLIENT_VERSION.into()),
            config_version: storage
                .get("cursorAuth/configVersion")
                .map(|v| coerce(v))
                .unwrap_or_default(),
            client_id: CURSOR_CLIENT_ID.to_string(),
        }),
    })
}

/// Import a Cursor token from the local desktop SQLite store.
///
/// # Errors
/// Returns an error when the store cannot be read or is missing tokens.
pub fn import_cursor_local(storage_path: &Path) -> Result<TokenData> {
    cursor_token_from_storage(&read_cursor_sqlite(storage_path)?)
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

    #[test]
    fn token_from_storage_builds_cursor_token() {
        use std::collections::BTreeMap;
        let mut s = BTreeMap::new();
        s.insert("cursorAuth/accessToken".to_string(), "jwt".to_string());
        s.insert("cursorAuth/refreshToken".to_string(), "rt".to_string());
        s.insert("cursorAuth/cachedEmail".to_string(), "u@x.com".to_string());
        s.insert("storage.serviceMachineId".to_string(), "machine-1".to_string());
        let token = cursor_token_from_storage(&s).expect("token");
        assert_eq!(token.refresh_token, "rt");
        assert_eq!(token.email, "u@x.com");
        assert_eq!(token.provider, crate::types::ProviderId::Cursor);
        assert_eq!(
            token.cursor.unwrap().service_machine_id.as_deref(),
            Some("machine-1")
        );
    }

    #[test]
    fn token_from_storage_requires_tokens() {
        use std::collections::BTreeMap;
        assert!(cursor_token_from_storage(&BTreeMap::new()).is_err());
    }

    #[test]
    fn import_reads_real_sqlite() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("state.vscdb");
        let conn = rusqlite::Connection::open(&db).expect("open");
        conn.execute("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)", [])
            .expect("create");
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
            rusqlite::params!["cursorAuth/accessToken", "jwt"],
        )
        .expect("ins1");
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
            rusqlite::params!["cursorAuth/refreshToken", "rt"],
        )
        .expect("ins2");
        drop(conn);
        let token = import_cursor_local(&db).expect("import");
        assert_eq!(token.refresh_token, "rt");
    }
}
