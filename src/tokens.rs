use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::types::{ProviderId, TokenData};
use crate::utils::{decode_jwt_payload, now_iso, sanitize_email};

#[derive(Debug, Serialize, Deserialize)]
struct StoredToken {
    access_token: String,
    refresh_token: String,
    email: Option<String>,
    #[serde(rename = "type")]
    token_type: Option<String>,
    expired: String,
    account_uuid: Option<String>,
    id_token: Option<String>,
    last_refresh: Option<String>,
    plan_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor_service_machine_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor_client_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor_config_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor_client_id: Option<String>,
}

/// Save an OAuth token under the provider-specific auth directory filename.
///
/// # Errors
///
/// Returns an error when the auth directory cannot be created, JSON cannot be encoded, or the token
/// file cannot be written/chmodded.
pub fn save_token(auth_dir: &Path, token: &TokenData) -> Result<PathBuf> {
    fs::create_dir_all(auth_dir)
        .with_context(|| format!("failed to create {}", auth_dir.display()))?;
    set_mode(auth_dir, 0o700)?;

    let filename = format!(
        "{}-{}.json",
        token.provider.storage_prefix(),
        sanitize_email(&token.email)
    );
    let path = auth_dir.join(filename);
    let stored = token_to_storage(token);
    fs::write(&path, serde_json::to_string_pretty(&stored)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    set_mode(&path, 0o600)?;
    Ok(path)
}

/// Load all readable provider token files from an auth directory.
///
/// Invalid token files are skipped, matching the Python implementation's permissive load behavior.
///
/// # Errors
///
/// Returns an error when the auth directory exists but cannot be read.
pub fn load_all_tokens(auth_dir: &Path, provider: Option<ProviderId>) -> Result<Vec<TokenData>> {
    if !auth_dir.exists() {
        return Ok(Vec::new());
    }

    let prefix = provider.map(ProviderId::storage_prefix);
    let mut paths = fs::read_dir(auth_dir)
        .with_context(|| format!("failed to read {}", auth_dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read {}", auth_dir.display()))?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();

    let mut tokens = Vec::new();
    for path in paths {
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(prefix) = prefix {
            if !filename.starts_with(&format!("{prefix}-")) {
                continue;
            }
        } else if !(filename.starts_with("claude-")
            || filename.starts_with("codex-")
            || filename.starts_with("opencodego-")
            || filename.starts_with("cursor-"))
        {
            continue;
        }

        let Some(stored) = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<StoredToken>(&text).ok())
        else {
            continue;
        };
        let token = storage_to_token(stored);
        if provider.is_none_or(|provider| token.provider == provider) {
            tokens.push(token);
        }
    }
    Ok(tokens)
}

fn token_to_storage(token: &TokenData) -> StoredToken {
    StoredToken {
        access_token: token.access_token.clone(),
        refresh_token: token.refresh_token.clone(),
        email: Some(token.email.clone()),
        token_type: Some(match token.provider {
            ProviderId::Anthropic => "claude".to_string(),
            ProviderId::Codex => "codex".to_string(),
            ProviderId::OpenCodeGo => "opencodego".to_string(),
            ProviderId::Cursor => "cursor".to_string(),
        }),
        expired: token.expires_at.clone(),
        account_uuid: Some(token.account_uuid.clone()),
        id_token: token.id_token.clone(),
        last_refresh: Some(token.last_refresh_at.clone().unwrap_or_else(now_iso)),
        plan_type: token.plan_type.clone(),
        cursor_service_machine_id: token
            .cursor
            .as_ref()
            .and_then(|c| c.service_machine_id.clone()),
        cursor_client_version: token.cursor.as_ref().map(|c| c.client_version.clone()),
        cursor_config_version: token.cursor.as_ref().map(|c| c.config_version.clone()),
        cursor_client_id: token.cursor.as_ref().map(|c| c.client_id.clone()),
    }
}

fn storage_to_token(stored: StoredToken) -> TokenData {
    let provider = match stored.token_type.as_deref() {
        Some("codex") => ProviderId::Codex,
        Some("opencodego") => ProviderId::OpenCodeGo,
        Some("cursor") => ProviderId::Cursor,
        _ => ProviderId::Anthropic,
    };
    let plan_type = stored
        .plan_type
        .clone()
        .or_else(|| plan_type_from_id_token(stored.id_token.as_deref()));
    TokenData {
        access_token: stored.access_token,
        refresh_token: stored.refresh_token,
        email: stored.email.unwrap_or_else(|| "unknown".to_string()),
        expires_at: stored.expired,
        account_uuid: stored.account_uuid.unwrap_or_default(),
        provider,
        id_token: stored.id_token,
        last_refresh_at: stored.last_refresh,
        plan_type,
        cursor: (provider == ProviderId::Cursor).then(|| crate::types::CursorMeta {
            service_machine_id: stored.cursor_service_machine_id,
            client_version: stored.cursor_client_version.unwrap_or_default(),
            config_version: stored.cursor_config_version.unwrap_or_default(),
            client_id: stored.cursor_client_id.unwrap_or_default(),
        }),
    }
}

fn plan_type_from_id_token(id_token: Option<&str>) -> Option<String> {
    let claims = decode_jwt_payload(id_token?).ok()?;
    let auth = claims.get("https://api.openai.com/auth");
    auth.and_then(|auth| auth.get("chatgpt_plan_type"))
        .or_else(|| claims.get("chatgpt_plan_type"))
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?
        .permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("failed to chmod {}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod cursor_tests {
    use super::*;
    use crate::types::{CursorMeta, ProviderId, TokenData};

    #[test]
    fn cursor_token_round_trips_with_meta() {
        let dir = tempfile::tempdir().expect("tempdir");
        let token = TokenData {
            access_token: "jwt".into(),
            refresh_token: "rt".into(),
            email: "u@cursor.local".into(),
            expires_at: "2030-01-01T00:00:00Z".into(),
            account_uuid: "uuid-1".into(),
            provider: ProviderId::Cursor,
            id_token: None,
            last_refresh_at: Some("2026-01-01T00:00:00Z".into()),
            plan_type: Some("pro".into()),
            cursor: Some(CursorMeta {
                service_machine_id: Some("machine-1".into()),
                client_version: "cli-x".into(),
                config_version: "cfg-1".into(),
                client_id: "cid-1".into(),
            }),
        };
        save_token(dir.path(), &token).expect("save");
        let loaded = load_all_tokens(dir.path(), Some(ProviderId::Cursor)).expect("load");
        assert_eq!(loaded, vec![token]);
    }
}
