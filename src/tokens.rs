use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::types::{ProviderId, ProviderKind, TokenData};
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
        token.provider.storage_dir(),
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
pub fn load_all_tokens(auth_dir: &Path, provider: Option<&ProviderId>) -> Result<Vec<TokenData>> {
    if !auth_dir.exists() {
        return Ok(Vec::new());
    }

    let prefix = provider.map(|provider| provider.storage_dir().to_string());
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
        // `claude-` is the v0.1 Anthropic filename prefix; keep accepting it until the Task 6
        // migration moves legacy files into per-id subdirectories.
        if let Some(prefix) = &prefix {
            let legacy_anthropic = prefix == "anthropic" && filename.starts_with("claude-");
            if !filename.starts_with(&format!("{prefix}-")) && !legacy_anthropic {
                continue;
            }
        } else if !(filename.starts_with("anthropic-")
            || filename.starts_with("claude-")
            || filename.starts_with("codex-")
            || filename.starts_with("opencode-"))
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
        if provider.is_none_or(|provider| token.provider.kind == provider.kind) {
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
        // The on-disk `token_type` discriminator is frozen at the v0.1 names ("claude" for
        // Anthropic, etc.) so files written by this version still load on older builds.
        // Filename prefixes (`anthropic-` etc.) are decoupled from this — see `save_token`.
        token_type: Some(match token.provider.kind {
            ProviderKind::Anthropic => "claude".to_string(),
            ProviderKind::Codex => "codex".to_string(),
            ProviderKind::Opencode => "opencode".to_string(),
        }),
        expired: token.expires_at.clone(),
        account_uuid: Some(token.account_uuid.clone()),
        id_token: token.id_token.clone(),
        last_refresh: Some(token.last_refresh_at.clone().unwrap_or_else(now_iso)),
        plan_type: token.plan_type.clone(),
    }
}

fn storage_to_token(stored: StoredToken) -> TokenData {
    let provider = match stored.token_type.as_deref() {
        Some("codex") => ProviderId::codex(),
        Some("opencode") => ProviderId::opencode(),
        _ => ProviderId::anthropic(),
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
mod tests {
    use super::{ProviderId, ProviderKind, TokenData, load_all_tokens, save_token};

    fn token(provider: ProviderId, email: &str) -> TokenData {
        TokenData {
            access_token: "a".into(),
            refresh_token: "r".into(),
            email: email.into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
            account_uuid: "u".into(),
            provider,
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
        }
    }

    #[test]
    fn save_token_uses_storage_dir_as_filename_prefix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path =
            save_token(dir.path(), &token(ProviderId::anthropic(), "alice@x.com")).expect("save");
        let filename = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            filename.starts_with("anthropic-"),
            "filename was {filename}"
        );
    }

    #[test]
    fn load_all_tokens_reads_legacy_claude_filename() {
        let dir = tempfile::tempdir().expect("tempdir");
        let legacy_path = dir.path().join("claude-alice@example.com.json");
        std::fs::write(
            &legacy_path,
            r#"{
                "access_token": "a",
                "refresh_token": "r",
                "email": "alice@example.com",
                "type": "claude",
                "expired": "2099-01-01T00:00:00Z",
                "account_uuid": "u"
            }"#,
        )
        .expect("write legacy fixture");

        let loaded = load_all_tokens(dir.path(), Some(&ProviderId::anthropic())).expect("load");
        assert_eq!(loaded.len(), 1, "expected one legacy token, got {loaded:?}");
        assert_eq!(loaded[0].provider.kind, ProviderKind::Anthropic);
        assert_eq!(loaded[0].email, "alice@example.com");
    }
}
