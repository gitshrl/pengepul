use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use pengepul::config::{CloakingConfig, Config, DebugMode, TimeoutConfig};
use pengepul::types::{AvailableAccount, ProviderId, TokenData};
use pengepul::upstream::{
    anthropic_headers, apply_cloaking, build_beta_header, codex_headers,
    normalize_codex_responses_body, opencode_go_headers,
};
use serde_json::{Value, json};

fn config() -> Config {
    let mut codex = BTreeMap::new();
    codex.insert("originator".to_string(), "test_codex".to_string());
    codex.insert("cli-version".to_string(), "1.2.3".to_string());
    codex.insert("openai-beta".to_string(), "responses=v1".to_string());

    Config {
        host: String::new(),
        port: 8317,
        auth_dir: PathBuf::from("/tmp/pengepul-test"),
        api_keys: HashSet::from(["sk-test".to_string()]),
        body_limit: "200mb".to_string(),
        cloaking: CloakingConfig {
            cli_version: "2.1.88".to_string(),
            entrypoint: "cli".to_string(),
            codex,
        },
        timeouts: TimeoutConfig {
            messages_ms: 120_000,
            stream_messages_ms: 600_000,
            count_tokens_ms: 30_000,
        },
        stats_enabled: true,
        debug: DebugMode::Off,
    }
}

fn account(provider: ProviderId) -> AvailableAccount {
    AvailableAccount {
        token: TokenData {
            access_token: format!("{provider}-access"),
            refresh_token: format!("{provider}-refresh"),
            email: format!("{provider}@example.com"),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
            account_uuid: format!("acct-{provider}"),
            provider,
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
            cursor: None,
        },
        device_id: "device-123".to_string(),
        account_uuid: format!("acct-{provider}"),
        provider,
        chatgpt_account_id: (provider == ProviderId::Codex).then(|| "chatgpt-account".to_string()),
    }
}

#[test]
fn anthropic_headers_include_cloaking_session_and_beta() {
    let mut request_headers = BTreeMap::new();
    request_headers.insert("authorization".to_string(), "Bearer sk-test".to_string());

    let headers = anthropic_headers(
        "anthropic-access",
        false,
        120_000,
        "claude-sonnet-4-6",
        &config(),
        &request_headers,
        false,
    );

    assert_eq!(headers["Authorization"], "Bearer anthropic-access");
    assert_eq!(headers["Accept"], "application/json");
    assert_eq!(headers["User-Agent"], "claude-cli/2.1.88 (external, cli)");
    assert_eq!(headers["X-Stainless-Timeout"], "120");
    assert!(headers["X-Claude-Code-Session-Id"].len() >= 32);
    assert!(headers["anthropic-beta"].contains("oauth-2025-04-20"));
    assert!(headers["anthropic-beta"].contains("advanced-tool-use-2025-11-20"));
}

#[test]
fn beta_header_switches_for_structured_and_haiku() {
    assert!(build_beta_header("claude-sonnet-4-6", true).contains("structured-outputs-2025-12-15"));
    assert!(build_beta_header("claude-haiku-4-5-20251001", false).contains("claude-code-20250219"));
    assert!(!build_beta_header("claude-haiku-4-5-20251001", false).contains("effort-2025-11-24"));
}

#[test]
fn apply_cloaking_injects_billing_prefix_and_metadata() {
    let body = json!({
        "messages": [{"role": "user", "content": "reply exactly: pong"}]
    });
    let mut request_headers = BTreeMap::new();
    request_headers.insert("authorization".to_string(), "Bearer sk-test".to_string());
    request_headers.insert(
        "x-claude-code-session-id".to_string(),
        "session-from-client".to_string(),
    );

    let cloaked = apply_cloaking(
        &body,
        &request_headers,
        &account(ProviderId::Anthropic),
        &config(),
    );

    let system = cloaked["system"].as_array().expect("system blocks");
    assert!(
        system[0]["text"]
            .as_str()
            .expect("billing text")
            .contains("x-anthropic-billing-header")
    );
    assert_eq!(
        system[1]["text"],
        "You are Claude Code, Anthropic's official CLI for Claude."
    );

    let user_id = cloaked["metadata"]["user_id"]
        .as_str()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .expect("metadata user id");
    assert_eq!(user_id["device_id"], "device-123");
    assert_eq!(user_id["account_uuid"], "acct-anthropic");
    assert_eq!(user_id["session_id"], "session-from-client");
}

#[test]
fn normalize_codex_responses_body_defaults_and_string_input() {
    let normalized = normalize_codex_responses_body(&json!({
        "model": "gpt-5.4",
        "input": "reply exactly: pong"
    }));

    assert_eq!(
        normalized["input"],
        json!([{"role": "user", "content": "reply exactly: pong"}])
    );
    assert_eq!(normalized["stream"], true);
    assert_eq!(normalized["store"], false);
    assert_eq!(normalized["instructions"], "");
}

#[test]
fn opencode_go_headers_use_bearer_auth() {
    let headers = opencode_go_headers("sk-go", false);
    assert_eq!(headers["Authorization"], "Bearer sk-go");
    assert_eq!(headers["Content-Type"], "application/json");
    assert_eq!(headers["Accept"], "application/json");

    let stream_headers = opencode_go_headers("sk-go", true);
    assert_eq!(stream_headers["Accept"], "text/event-stream");
}

#[test]
fn codex_headers_include_account_and_cloaking() {
    let headers = codex_headers(&account(ProviderId::Codex), true, &config());

    assert_eq!(headers["Authorization"], "Bearer codex-access");
    assert_eq!(headers["Accept"], "text/event-stream");
    assert_eq!(headers["originator"], "test_codex");
    assert_eq!(headers["version"], "1.2.3");
    assert_eq!(headers["OpenAI-Beta"], "responses=v1");
    assert_eq!(headers["ChatGPT-Account-ID"], "chatgpt-account");
    assert!(headers["User-Agent"].starts_with("test_codex/1.2.3 ("));
}
