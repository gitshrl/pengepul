use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use pengepul::config::{load_config, selected_config_path};
use pengepul::oauth::{
    CODEX_CALLBACK_PATH, CODEX_CALLBACK_PORT, CODEX_CLIENT_ID, detect_exhausted_reason,
    generate_anthropic_auth_url, generate_codex_auth_url,
};
use pengepul::providers::build_registry;
use pengepul::tokens::{load_all_tokens, save_token};
use pengepul::translate::resolve_model;
use pengepul::types::{PkceCodes, ProviderId, TokenData};
use tempfile::tempdir;

fn walk_json_files(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(root).expect("read dir") {
        let entry = entry.expect("entry");
        if !entry.file_type().expect("file type").is_dir() {
            continue;
        }
        for sub in fs::read_dir(entry.path()).expect("read subdir") {
            let path = sub.expect("sub entry").path();
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
            {
                out.push(path.strip_prefix(root).expect("under root").to_path_buf());
            }
        }
    }
    out
}

fn jwt(payload: &serde_json::Value) -> String {
    use base64::Engine;

    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header = engine.encode(serde_json::json!({"alg": "none"}).to_string());
    let payload = engine.encode(payload.to_string());
    format!("{header}.{payload}.")
}

#[test]
fn default_config_is_generated_under_home_pengepul() {
    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    fs::create_dir(&workspace).expect("workspace");

    let config = load_config(None, Some(tmp.path()), &workspace).expect("load config");

    let config_path = tmp.path().join(".pengepul/config.yaml");
    assert!(config_path.exists());
    assert!(!workspace.join("config.yaml").exists());
    assert_eq!(config.auth_dir, tmp.path().join(".pengepul"));
    assert_eq!(
        config_path
            .parent()
            .unwrap()
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        config_path.metadata().unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn oauth_detects_exhausted_refresh_markers() {
    assert_eq!(
        detect_exhausted_reason(r#"{"error":"invalid_grant"}"#),
        Some("invalid_grant")
    );
    assert_eq!(
        detect_exhausted_reason("refresh_token_reused by another client"),
        Some("refresh_token_reused")
    );
    assert_eq!(detect_exhausted_reason("temporary outage"), None);
}

#[test]
fn default_config_migrates_legacy_workspace_config() {
    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    fs::create_dir(&workspace).expect("workspace");
    fs::write(
        workspace.join("config.yaml"),
        r#"host: "127.0.0.1"
port: 9000
auth-dir: ~/.pengepul
api-keys:
  - sk-legacy
"#,
    )
    .expect("legacy config");

    let config = load_config(None, Some(tmp.path()), &workspace).expect("load config");

    assert_eq!(config.api_keys, ["sk-legacy".to_string()].into());
    assert_eq!(config.port, 9000);
    assert!(tmp.path().join(".pengepul/config.yaml").exists());
    assert!(workspace.join("config.yaml").exists());
}

#[test]
fn explicit_config_path_is_respected() {
    let tmp = tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let config_path = tmp.path().join("custom.yaml");

    let config = load_config(Some(&config_path), Some(&home), tmp.path()).expect("load config");

    assert!(config_path.exists());
    assert!(!home.join(".pengepul/config.yaml").exists());
    assert_eq!(config.auth_dir, home.join(".pengepul"));
    assert_eq!(
        selected_config_path(Some(&config_path), Some(&home), tmp.path()),
        config_path
    );
}

#[test]
fn token_storage_round_trips_provider_files() {
    let tmp = tempdir().expect("tempdir");

    save_token(
        tmp.path(),
        &TokenData {
            access_token: "claude-access".to_string(),
            refresh_token: "claude-refresh".to_string(),
            email: "alice@example.com".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
            account_uuid: "acct-claude".to_string(),
            provider: "anthropic".parse().unwrap(),
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
        },
    )
    .expect("save anthropic");
    save_token(
        tmp.path(),
        &TokenData {
            access_token: "codex-access".to_string(),
            refresh_token: "codex-refresh".to_string(),
            email: "bob@example.com".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
            account_uuid: "acct-codex".to_string(),
            provider: "codex".parse().unwrap(),
            id_token: Some(jwt(&serde_json::json!({"email": "bob@example.com"}))),
            last_refresh_at: None,
            plan_type: None,
        },
    )
    .expect("save codex");
    save_token(
        tmp.path(),
        &TokenData {
            access_token: "opencode-key".to_string(),
            refresh_token: String::new(),
            email: "opencode-acct".to_string(),
            expires_at: "9999-12-31T23:59:59Z".to_string(),
            account_uuid: String::new(),
            provider: "opencode".parse().unwrap(),
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
        },
    )
    .expect("save opencode");

    let mut files = walk_json_files(tmp.path());
    files.sort();
    assert_eq!(
        files,
        [
            PathBuf::from("anthropic").join("alice@example.com.json"),
            PathBuf::from("codex").join("bob@example.com.json"),
            PathBuf::from("opencode").join("opencode-acct.json"),
        ]
    );
    let anthropic: ProviderId = "anthropic".parse().unwrap();
    let codex: ProviderId = "codex".parse().unwrap();
    let opencode: ProviderId = "opencode".parse().unwrap();
    assert_eq!(
        load_all_tokens(tmp.path(), Some(&anthropic))
            .expect("load anthropic")
            .into_iter()
            .map(|token| token.email)
            .collect::<Vec<_>>(),
        ["alice@example.com"]
    );
    assert_eq!(
        load_all_tokens(tmp.path(), Some(&codex))
            .expect("load codex")
            .into_iter()
            .map(|token| token.email)
            .collect::<Vec<_>>(),
        ["bob@example.com"]
    );
    assert_eq!(
        load_all_tokens(tmp.path(), Some(&opencode))
            .expect("load opencode")
            .into_iter()
            .map(|token| token.email)
            .collect::<Vec<_>>(),
        ["opencode-acct"]
    );
}

#[test]
fn registry_routes_anthropic_codex_and_opencode() {
    let tmp = tempdir().expect("tempdir");
    let registry = build_registry(tmp.path());

    assert_eq!(
        registry
            .all()
            .iter()
            .map(|provider| provider.id.clone())
            .collect::<Vec<_>>(),
        [
            "anthropic".parse().unwrap(),
            "codex".parse().unwrap(),
            "opencode".parse().unwrap()
        ]
    );
    assert_eq!(
        registry.for_model("claude-sonnet-4-6").id,
        "anthropic".parse().unwrap()
    );
    assert_eq!(
        registry.for_model("sonnet").id,
        "anthropic".parse().unwrap()
    );
    assert_eq!(registry.for_model("gpt-5").id, "codex".parse().unwrap());
    assert_eq!(
        registry.for_model("gpt-5.4-mini").id,
        "codex".parse().unwrap()
    );
    assert_eq!(registry.for_model("o4-mini").id, "codex".parse().unwrap());
    assert_eq!(
        registry.for_model("codex-mini-latest").id,
        "codex".parse().unwrap()
    );
    assert_eq!(
        registry.for_model("gpt-4o").id,
        "anthropic".parse().unwrap()
    );
    assert_eq!(
        registry.for_model("custom-model").id,
        "anthropic".parse().unwrap()
    );
    assert_eq!(
        registry.for_model("opencode/glm-5.1").id,
        "opencode".parse().unwrap()
    );
    // a bare opencode model id (no routing prefix) must not hijack the default.
    assert_eq!(
        registry.for_model("glm-5.1").id,
        "anthropic".parse().unwrap()
    );
}

#[test]
fn resolve_model_aliases() {
    assert_eq!(resolve_model(None), "claude-sonnet-4-6");
    assert_eq!(resolve_model(Some("opus")), "claude-opus-4-8");
    assert_eq!(resolve_model(Some("sonnet")), "claude-sonnet-4-6");
    assert_eq!(resolve_model(Some("haiku")), "claude-haiku-4-5-20251001");
    assert_eq!(resolve_model(Some("gpt-5.4")), "gpt-5.4");
}

#[test]
fn oauth_urls_use_expected_callback_and_scope() {
    let pkce = PkceCodes {
        code_verifier: "verifier".to_string(),
        code_challenge: "challenge".to_string(),
    };

    let anthropic = url::Url::parse(&generate_anthropic_auth_url("state", &pkce)).unwrap();
    let anthropic_query = anthropic.query_pairs().into_owned().collect::<Vec<_>>();
    assert_eq!(anthropic.domain(), Some("claude.ai"));
    assert!(anthropic_query.contains(&(
        "redirect_uri".to_string(),
        "http://localhost:54545/callback".to_string(),
    )));
    assert!(anthropic_query.contains(&(
        "scope".to_string(),
        "org:create_api_key user:profile user:inference".to_string(),
    )));

    let codex = url::Url::parse(&generate_codex_auth_url("state", &pkce)).unwrap();
    let codex_query = codex.query_pairs().into_owned().collect::<Vec<_>>();
    assert_eq!(codex.domain(), Some("auth.openai.com"));
    assert!(codex_query.contains(&(
        "redirect_uri".to_string(),
        format!("http://localhost:{CODEX_CALLBACK_PORT}{CODEX_CALLBACK_PATH}"),
    )));
    assert!(codex_query.contains(&("client_id".to_string(), CODEX_CLIENT_ID.to_string())));
    assert!(codex_query.contains(&("originator".to_string(), "codex_cli_rs".to_string())));
    assert!(codex_query.contains(&("code_challenge".to_string(), "challenge".to_string())));
}
