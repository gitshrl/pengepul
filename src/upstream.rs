use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use rand::Rng;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::config::Config;
use crate::types::AvailableAccount;
use crate::utils::sha256_hex;

pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
pub const ANTHROPIC_OAUTH_BETA: &str = "oauth-2025-04-20";
pub const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
pub const CODEX_RESPONSES_PATH: &str = "/codex/responses";
pub const CODEX_MODELS_PATH: &str = "/codex/models";
pub const CODEX_DEFAULT_ORIGINATOR: &str = "codex_cli_rs";
pub const CODEX_DEFAULT_CLI_VERSION: &str = "0.125.0";

const FINGERPRINT_SALT: &str = "59cf53e54c78";

type Sessions = BTreeMap<String, (String, Instant, Duration)>;

static SESSIONS: OnceLock<Mutex<Sessions>> = OnceLock::new();

#[must_use]
pub fn build_beta_header(model: &str, structured: bool) -> String {
    let is_haiku = model.contains("haiku");
    let mut common = vec![
        "oauth-2025-04-20",
        "interleaved-thinking-2025-05-14",
        "redact-thinking-2026-02-12",
        "context-management-2025-06-27",
        "prompt-caching-scope-2026-01-05",
    ];
    let extra = if structured {
        vec!["structured-outputs-2025-12-15"]
    } else if is_haiku {
        vec!["claude-code-20250219"]
    } else {
        vec!["advanced-tool-use-2025-11-20", "effort-2025-11-24"]
    };
    if !is_haiku && !structured {
        common.insert(0, "claude-code-20250219");
    }
    common
        .into_iter()
        .chain(extra)
        .collect::<Vec<_>>()
        .join(",")
}

#[must_use]
pub fn anthropic_headers(
    token: &str,
    stream: bool,
    timeout_ms: u64,
    model: &str,
    config: &Config,
    request_headers: &BTreeMap<String, String>,
    structured: bool,
) -> BTreeMap<String, String> {
    let api_hash = sha256_hex(&extract_api_key(request_headers).unwrap_or_default());
    let mut headers = BTreeMap::from([
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Authorization".to_string(), format!("Bearer {token}")),
        (
            "User-Agent".to_string(),
            format!(
                "claude-cli/{} (external, {})",
                config.cloaking.cli_version, config.cloaking.entrypoint
            ),
        ),
        (
            "X-Claude-Code-Session-Id".to_string(),
            session_id(&api_hash),
        ),
        ("X-Stainless-Lang".to_string(), "js".to_string()),
        (
            "X-Stainless-Package-Version".to_string(),
            "0.74.0".to_string(),
        ),
        ("X-Stainless-Runtime".to_string(), "node".to_string()),
        (
            "X-Stainless-Runtime-Version".to_string(),
            "v22.13.0".to_string(),
        ),
        ("X-Stainless-Arch".to_string(), stainless_arch()),
        ("X-Stainless-Os".to_string(), stainless_os()),
        (
            "X-Stainless-Timeout".to_string(),
            timeout_seconds(timeout_ms).to_string(),
        ),
        ("X-Stainless-Retry-Count".to_string(), "0".to_string()),
        (
            "Accept".to_string(),
            if stream {
                "text/event-stream".to_string()
            } else {
                "application/json".to_string()
            },
        ),
        (
            "anthropic-dangerous-direct-browser-access".to_string(),
            "true".to_string(),
        ),
        ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ("x-app".to_string(), "cli".to_string()),
        (
            "x-client-request-id".to_string(),
            Uuid::new_v4().to_string(),
        ),
    ]);
    headers.extend(passthrough_anthropic_headers(request_headers));

    if let Some(beta) = headers.get("anthropic-beta").cloned() {
        let mut parts = beta
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if !parts.iter().any(|part| part == ANTHROPIC_OAUTH_BETA) {
            parts.insert(0, ANTHROPIC_OAUTH_BETA.to_string());
        }
        parts.dedup();
        headers.insert("anthropic-beta".to_string(), parts.join(","));
    } else {
        headers.insert(
            "anthropic-beta".to_string(),
            build_beta_header(model, structured),
        );
    }

    headers
}

#[must_use]
pub fn apply_cloaking(
    body: &Value,
    request_headers: &BTreeMap<String, String>,
    account: &AvailableAccount,
    config: &Config,
) -> Value {
    let mut next_body = body.clone();
    if !next_body.is_object() {
        next_body = json!({});
    }
    let Some(object) = next_body.as_object_mut() else {
        return next_body;
    };
    let existing = object
        .get("system")
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    let remaining = match existing {
        Value::Array(values) => values,
        Value::Null => Vec::new(),
        other => vec![json!({"type": "text", "text": other.to_string()})],
    };

    let mut billing = None;
    let mut prefix = None;
    let mut kept = Vec::new();
    for block in remaining {
        let text = block.get("text").and_then(Value::as_str).unwrap_or("");
        if text.contains("x-anthropic-billing-header") && billing.is_none() {
            billing = Some(block);
        } else if text.contains("You are Claude Code") && prefix.is_none() {
            prefix = Some(block);
        } else {
            kept.push(block);
        }
    }

    let billing = billing.unwrap_or_else(|| {
        json!({
            "type": "text",
            "text": billing_header(
                object
                    .get("messages")
                    .and_then(Value::as_array)
                    .map_or(&[], Vec::as_slice),
                &config.cloaking.cli_version,
                &config.cloaking.entrypoint,
            )
        })
    });
    let prefix = prefix.unwrap_or_else(|| {
        json!({
            "type": "text",
            "text": "You are Claude Code, Anthropic's official CLI for Claude.",
            "cache_control": {"type": "ephemeral"}
        })
    });
    object.insert(
        "system".to_string(),
        Value::Array([billing, prefix].into_iter().chain(kept).collect()),
    );

    let session = header_value(request_headers, "x-claude-code-session-id").map_or_else(
        || {
            let api_hash = sha256_hex(&extract_api_key(request_headers).unwrap_or_default());
            session_id(&api_hash)
        },
        ToOwned::to_owned,
    );
    let metadata = object.entry("metadata").or_insert_with(|| json!({}));
    if !metadata.is_object() {
        *metadata = json!({});
    }
    metadata["user_id"] = Value::String(
        json!({
            "device_id": account.device_id,
            "account_uuid": account.account_uuid,
            "session_id": session
        })
        .to_string(),
    );
    next_body
}

#[must_use]
pub fn normalize_codex_responses_body(body: &Value) -> Value {
    let mut next_body = body.clone();
    if !next_body.is_object() {
        next_body = json!({});
    }
    let Some(object) = next_body.as_object_mut() else {
        return next_body;
    };
    if let Some(input) = object
        .get("input")
        .filter(|value| value.is_string())
        .cloned()
    {
        object.insert(
            "input".to_string(),
            json!([{"role": "user", "content": input}]),
        );
    }
    object.entry("stream").or_insert(Value::Bool(true));
    object.entry("store").or_insert(Value::Bool(false));
    object
        .entry("instructions")
        .or_insert_with(|| Value::String(String::new()));
    next_body
}

#[must_use]
pub fn codex_headers(
    account: &AvailableAccount,
    stream: bool,
    config: &Config,
) -> BTreeMap<String, String> {
    let originator = config
        .cloaking
        .codex
        .get("originator")
        .map_or(CODEX_DEFAULT_ORIGINATOR, String::as_str);
    let version = config
        .cloaking
        .codex
        .get("cli-version")
        .map_or(CODEX_DEFAULT_CLI_VERSION, String::as_str);
    let mut headers = BTreeMap::from([
        ("Content-Type".to_string(), "application/json".to_string()),
        (
            "Authorization".to_string(),
            format!("Bearer {}", account.token.access_token),
        ),
        (
            "Accept".to_string(),
            if stream {
                "text/event-stream".to_string()
            } else {
                "application/json".to_string()
            },
        ),
        ("User-Agent".to_string(), codex_user_agent(config)),
        ("originator".to_string(), originator.to_string()),
        ("version".to_string(), version.to_string()),
    ]);
    if let Some(account_id) = &account.chatgpt_account_id {
        headers.insert("ChatGPT-Account-ID".to_string(), account_id.clone());
    }
    if let Some(beta) = config.cloaking.codex.get("openai-beta") {
        headers.insert("OpenAI-Beta".to_string(), beta.clone());
    }
    headers
}

fn session_id(api_key_hash: &str) -> String {
    let now = Instant::now();
    let sessions = SESSIONS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut sessions = sessions.lock().expect("session cache lock is poisoned");
    if let Some((session_id, last_used, ttl)) = sessions.get_mut(api_key_hash)
        && now.duration_since(*last_used) < *ttl
    {
        *last_used = now;
        return session_id.clone();
    }
    sessions.retain(|_, (_, last_used, ttl)| now.duration_since(*last_used) < *ttl);
    let ttl = Duration::from_secs(rand::rng().random_range(30 * 60..=300 * 60));
    let session_id = Uuid::new_v4().to_string();
    sessions.insert(api_key_hash.to_string(), (session_id.clone(), now, ttl));
    session_id
}

fn passthrough_anthropic_headers(
    request_headers: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let user_agent = header_value(request_headers, "user-agent").unwrap_or("");
    if !user_agent.to_ascii_lowercase().starts_with("claude-cli") {
        return BTreeMap::new();
    }

    let mut headers = request_headers
        .iter()
        .filter(|(key, _)| key.to_ascii_lowercase().starts_with("anthropic"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    if let Some(session) = header_value(request_headers, "x-claude-code-session-id") {
        headers.insert("X-Claude-Code-Session-Id".to_string(), session.to_string());
    }
    headers
}

fn billing_header(messages: &[Value], version: &str, entrypoint: &str) -> String {
    let text = first_user_text(messages);
    let chars = text.chars().collect::<Vec<_>>();
    let selected = [4_usize, 7, 20]
        .into_iter()
        .map(|index| chars.get(index).copied().unwrap_or('0'))
        .collect::<String>();
    let fingerprint = &sha256_hex(&format!("{FINGERPRINT_SALT}{selected}{version}"))[..3];
    format!(
        "x-anthropic-billing-header: cc_version={version}.{fingerprint}; cc_entrypoint={entrypoint};"
    )
}

fn first_user_text(messages: &[Value]) -> String {
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(content) = message.get("content") else {
            return String::new();
        };
        if let Some(text) = content.as_str() {
            return text.to_string();
        }
        if let Some(blocks) = content.as_array()
            && let Some(text) = blocks
                .iter()
                .find(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .and_then(|block| block.get("text"))
                .and_then(Value::as_str)
        {
            return text.to_string();
        }
    }
    String::new()
}

fn extract_api_key(headers: &BTreeMap<String, String>) -> Option<String> {
    header_value(headers, "authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            header_value(headers, "x-api-key")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
}

fn header_value<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn timeout_seconds(timeout_ms: u64) -> u64 {
    timeout_ms.div_ceil(1000).max(1)
}

fn stainless_os() -> String {
    match std::env::consts::OS {
        "macos" => "MacOS",
        "windows" => "Windows",
        "freebsd" => "FreeBSD",
        _ => "Linux",
    }
    .to_string()
}

fn stainless_arch() -> String {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        _ => "x86",
    }
    .to_string()
}

fn codex_user_agent(config: &Config) -> String {
    if let Some(user_agent) = config.cloaking.codex.get("user-agent") {
        return user_agent.clone();
    }
    let originator = config
        .cloaking
        .codex
        .get("originator")
        .map_or(CODEX_DEFAULT_ORIGINATOR, String::as_str);
    let version = config
        .cloaking
        .codex
        .get("cli-version")
        .map_or(CODEX_DEFAULT_CLI_VERSION, String::as_str);
    format!("{originator}/{version} ({}; {})", codex_os(), codex_arch())
}

fn codex_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    }
}

fn codex_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        _ => "x86_64",
    }
}
