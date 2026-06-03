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

    let provider_dir = auth_dir.join(token.provider.storage_dir());
    fs::create_dir_all(&provider_dir)
        .with_context(|| format!("failed to create {}", provider_dir.display()))?;
    set_mode(&provider_dir, 0o700)?;

    let filename = format!("{}.json", sanitize_email(&token.email));
    let path = provider_dir.join(filename);
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

    let scan_dirs: Vec<PathBuf> = if let Some(provider) = provider {
        vec![auth_dir.join(provider.storage_dir())]
    } else {
        // Scan every direct subdirectory; flat legacy files in auth_dir are handled by Task 6's
        // migration (not by this read path).
        let mut dirs = Vec::new();
        for entry in fs::read_dir(auth_dir)
            .with_context(|| format!("failed to read {}", auth_dir.display()))?
        {
            let entry =
                entry.with_context(|| format!("failed to read entry in {}", auth_dir.display()))?;
            let file_type = entry
                .file_type()
                .with_context(|| format!("failed to stat {}", entry.path().display()))?;
            if file_type.is_dir() {
                dirs.push(entry.path());
            }
        }
        dirs
    };

    let mut tokens = Vec::new();
    for provider_dir in scan_dirs {
        if !provider_dir.exists() {
            continue;
        }
        let mut paths = fs::read_dir(&provider_dir)
            .with_context(|| format!("failed to read {}", provider_dir.display()))?
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("failed to read {}", provider_dir.display()))?
            .into_iter()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect::<Vec<_>>();
        paths.sort();

        for path in paths {
            let stored = match fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))
                .and_then(|text| {
                    serde_json::from_str::<StoredToken>(&text)
                        .with_context(|| format!("failed to parse {}", path.display()))
                }) {
                Ok(stored) => stored,
                Err(error) => {
                    tracing::warn!(?error, path = %path.display(), "skipping unreadable token file");
                    continue;
                }
            };
            let token = storage_to_token(stored);
            if provider.is_none_or(|p| token.provider.kind == p.kind) {
                tokens.push(token);
            }
        }
    }
    Ok(tokens)
}

/// Move legacy flat-layout token files into per-id subdirectories. Returns the number of files
/// moved. Safe to call on every startup — no-op once layout is current.
///
/// Mapping: `claude-<rest>.json` → `anthropic/<rest>.json`,
/// `codex-<rest>.json` → `codex/<rest>.json`,
/// `opencode-<rest>.json` → `opencode/<rest>.json`.
///
/// # Errors
///
/// Returns an error if a destination subdirectory cannot be created or a file cannot be moved.
pub fn migrate_legacy_layout(auth_dir: &Path) -> Result<usize> {
    if !auth_dir.exists() {
        return Ok(0);
    }
    let mappings = [
        ("claude-", "anthropic"),
        ("codex-", "codex"),
        ("opencode-", "opencode"),
    ];
    let mut moved = 0;
    for entry in
        fs::read_dir(auth_dir).with_context(|| format!("failed to read {}", auth_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        {
            continue;
        }
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        for (prefix, dest_dir) in mappings {
            if let Some(rest) = filename.strip_prefix(prefix) {
                let dest = auth_dir.join(dest_dir);
                fs::create_dir_all(&dest)
                    .with_context(|| format!("failed to create {}", dest.display()))?;
                set_mode(&dest, 0o700)?;
                let new_path = dest.join(rest);
                fs::rename(&path, &new_path).with_context(|| {
                    format!(
                        "failed to move {} -> {}",
                        path.display(),
                        new_path.display()
                    )
                })?;
                moved += 1;
                tracing::info!(from = %path.display(), to = %new_path.display(), "migrated legacy token file");
                break;
            }
        }
    }
    Ok(moved)
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
    use super::{
        ProviderId, ProviderKind, TokenData, load_all_tokens, migrate_legacy_layout, save_token,
    };

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
    fn save_token_writes_under_per_id_subdirectory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path =
            save_token(dir.path(), &token(ProviderId::anthropic(), "alice@x.com")).expect("save");
        let relative = path.strip_prefix(dir.path()).expect("under dir");
        let components: Vec<_> = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        assert_eq!(components.len(), 2, "{components:?}");
        assert_eq!(components[0], "anthropic");
        assert!(
            std::path::Path::new(&components[1])
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        );
    }

    #[test]
    fn load_all_tokens_reads_per_id_subdirectories() {
        let dir = tempfile::tempdir().expect("tempdir");
        save_token(dir.path(), &token(ProviderId::anthropic(), "alice@x.com"))
            .expect("save anthropic");
        save_token(dir.path(), &token(ProviderId::codex(), "bob@y.com")).expect("save codex");

        let all = load_all_tokens(dir.path(), None).expect("load");
        assert_eq!(all.len(), 2);
        let kinds: Vec<_> = all.iter().map(|t| t.provider.kind).collect();
        assert!(kinds.contains(&ProviderKind::Anthropic));
        assert!(kinds.contains(&ProviderKind::Codex));

        let just_anthropic =
            load_all_tokens(dir.path(), Some(&ProviderId::anthropic())).expect("load anthropic");
        assert_eq!(just_anthropic.len(), 1);
        assert_eq!(just_anthropic[0].provider.kind, ProviderKind::Anthropic);
    }

    #[test]
    fn migrate_legacy_layout_moves_files_into_subdirs_idempotently() {
        let dir = tempfile::tempdir().expect("tempdir");
        let auth = dir.path();
        std::fs::create_dir_all(auth).unwrap();
        std::fs::write(
            auth.join("claude-alice_at_x.com.json"),
            r#"{"access_token":"a","refresh_token":"r","expired":"2099-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        std::fs::write(auth.join("codex-bob_at_y.com.json"), r#"{"access_token":"a","refresh_token":"r","expired":"2099-01-01T00:00:00Z","type":"codex"}"#).unwrap();
        std::fs::write(auth.join("opencode-deadbeef.json"), r#"{"access_token":"a","refresh_token":"","expired":"9999-12-31T23:59:59Z","type":"opencode"}"#).unwrap();
        std::fs::write(auth.join("unrelated.txt"), "ignore me").unwrap();

        let moved = migrate_legacy_layout(auth).expect("migrate");
        assert_eq!(moved, 3);

        assert!(auth.join("anthropic/alice_at_x.com.json").exists());
        assert!(auth.join("codex/bob_at_y.com.json").exists());
        assert!(auth.join("opencode/deadbeef.json").exists());
        assert!(!auth.join("claude-alice_at_x.com.json").exists());
        assert!(auth.join("unrelated.txt").exists());

        // Idempotent: second call moves nothing.
        let again = migrate_legacy_layout(auth).expect("migrate again");
        assert_eq!(again, 0);
    }
}
