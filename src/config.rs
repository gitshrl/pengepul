use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::utils::{generate_api_key, resolve_auth_dir};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeoutConfig {
    pub messages_ms: u64,
    pub stream_messages_ms: u64,
    pub count_tokens_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloakingConfig {
    pub cli_version: String,
    pub entrypoint: String,
    pub codex: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub auth_dir: PathBuf,
    pub api_keys: HashSet<String>,
    pub body_limit: String,
    pub cloaking: CloakingConfig,
    pub timeouts: TimeoutConfig,
    pub stats_enabled: bool,
    pub debug: DebugMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugMode {
    Off,
    Errors,
    Verbose,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    host: String,
    port: u16,
    #[serde(rename = "auth-dir")]
    auth_dir: String,
    #[serde(rename = "api-keys")]
    api_keys: Vec<String>,
    #[serde(rename = "body-limit")]
    body_limit: String,
    cloaking: RawCloaking,
    timeouts: RawTimeouts,
    stats: RawStats,
    debug: serde_yaml::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawCloaking {
    #[serde(rename = "cli-version")]
    cli_version: String,
    entrypoint: String,
    codex: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawTimeouts {
    #[serde(rename = "messages-ms")]
    messages: u64,
    #[serde(rename = "stream-messages-ms")]
    stream_messages: u64,
    #[serde(rename = "count-tokens-ms")]
    count_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawStats {
    enabled: bool,
}

impl Default for RawConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 8317,
            auth_dir: "~/.pengepul".to_string(),
            api_keys: Vec::new(),
            body_limit: "200mb".to_string(),
            cloaking: RawCloaking::default(),
            timeouts: RawTimeouts::default(),
            stats: RawStats::default(),
            debug: serde_yaml::Value::String("off".to_string()),
        }
    }
}

impl Default for RawCloaking {
    fn default() -> Self {
        Self {
            cli_version: "2.1.88".to_string(),
            entrypoint: "cli".to_string(),
            codex: std::collections::BTreeMap::new(),
        }
    }
}

impl Default for RawTimeouts {
    fn default() -> Self {
        Self {
            messages: 120_000,
            stream_messages: 600_000,
            count_tokens: 30_000,
        }
    }
}

impl Default for RawStats {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[must_use]
pub fn default_config_path(home: &Path) -> PathBuf {
    home.join(".pengepul/config.yaml")
}

#[must_use]
pub fn legacy_config_path(cwd: &Path) -> PathBuf {
    cwd.join("config.yaml")
}

#[must_use]
pub fn selected_config_path(
    config_path: Option<&Path>,
    home_override: Option<&Path>,
    cwd: &Path,
) -> PathBuf {
    config_paths(config_path, home_override, cwd).1
}

/// Load config from an explicit path, the default home config, or legacy workspace config.
///
/// # Errors
///
/// Returns an error when `HOME` is unavailable, config YAML is invalid, or the generated/migrated
/// config file cannot be written.
pub fn load_config(
    config_path: Option<&Path>,
    home_override: Option<&Path>,
    cwd: &Path,
) -> Result<Config> {
    let home = home_override
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .context("HOME is not set")?;
    let (read_path, write_path) = config_paths(config_path, Some(&home), cwd);
    let mut raw = if read_path.exists() {
        let text = fs::read_to_string(&read_path)
            .with_context(|| format!("failed to read {}", read_path.display()))?;
        if text.trim().is_empty() {
            RawConfig::default()
        } else {
            serde_yaml::from_str::<RawConfig>(&text)
                .with_context(|| format!("{} must contain a YAML mapping", read_path.display()))?
        }
    } else {
        RawConfig::default()
    };

    if raw.api_keys.is_empty() {
        raw.api_keys.push(generate_api_key());
        write_config(&write_path, &raw, config_path.is_none())?;
        println!(
            "\ngenerated API key and saved it to {}:\n\n  {}\n",
            write_path.display(),
            raw.api_keys[0]
        );
    } else if write_path != read_path {
        write_config(&write_path, &raw, true)?;
    }

    Ok(Config {
        host: raw.host,
        port: raw.port,
        auth_dir: resolve_auth_dir(&raw.auth_dir, &home),
        api_keys: raw.api_keys.into_iter().collect(),
        body_limit: raw.body_limit,
        cloaking: CloakingConfig {
            cli_version: raw.cloaking.cli_version,
            entrypoint: raw.cloaking.entrypoint,
            codex: raw.cloaking.codex,
        },
        timeouts: TimeoutConfig {
            messages_ms: raw.timeouts.messages,
            stream_messages_ms: raw.timeouts.stream_messages,
            count_tokens_ms: raw.timeouts.count_tokens,
        },
        stats_enabled: raw.stats.enabled,
        debug: normalize_debug(&raw.debug),
    })
}

fn config_paths(
    config_path: Option<&Path>,
    home_override: Option<&Path>,
    cwd: &Path,
) -> (PathBuf, PathBuf) {
    if let Some(path) = config_path {
        let expanded = expand_path(path, home_override);
        return (expanded.clone(), expanded);
    }

    let home = home_override
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    let default_path = default_config_path(&home);
    if default_path.exists() {
        return (default_path.clone(), default_path);
    }

    let legacy_path = legacy_config_path(cwd);
    if legacy_path.exists() {
        return (legacy_path, default_path);
    }

    (default_path.clone(), default_path)
}

fn expand_path(path: &Path, home: Option<&Path>) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return home.map_or_else(|| PathBuf::from("~"), Path::to_path_buf);
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home.map_or_else(|| path.to_path_buf(), |home| home.join(rest));
    }
    path.to_path_buf()
}

fn normalize_debug(value: &serde_yaml::Value) -> DebugMode {
    match value {
        serde_yaml::Value::Bool(true) => DebugMode::Errors,
        serde_yaml::Value::String(value) if value == "errors" => DebugMode::Errors,
        serde_yaml::Value::String(value) if value == "verbose" => DebugMode::Verbose,
        _ => DebugMode::Off,
    }
}

fn write_config(path: &Path, raw: &RawConfig, private_parent: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        if private_parent {
            set_mode(parent, 0o700)?;
        }
    } else {
        bail!("config path has no parent: {}", path.display());
    }

    let text = serde_yaml::to_string(raw).context("failed to encode config YAML")?;
    fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))?;
    set_mode(path, 0o600)
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
