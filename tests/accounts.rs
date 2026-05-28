use std::fs;
use std::sync::{Arc, Mutex};

use pengepul::accounts::{AccountManager, RefreshPolicy, RefreshPolicyKind};
use pengepul::types::{RefreshTokenExhaustedError, TokenData};
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn since_last_refresh_refreshes_legacy_token_without_last_refresh() {
    let tmp = tempdir().expect("tempdir");
    fs::write(
        tmp.path().join("codex-bob_example_com.json"),
        json!({
            "access_token": "old-access",
            "refresh_token": "old-refresh",
            "email": "bob@example.com",
            "type": "codex",
            "expired": "2030-01-01T00:00:00Z",
            "account_uuid": "acct-codex"
        })
        .to_string(),
    )
    .expect("write token");
    let refresh_calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured = Arc::clone(&refresh_calls);

    let mut manager = AccountManager::new(
        tmp.path().to_path_buf(),
        "codex".parse().unwrap(),
        move |refresh_token| {
            let captured = Arc::clone(&captured);
            Box::pin(async move {
                captured.lock().unwrap().push(refresh_token);
                Ok(TokenData {
                    access_token: "new-access".to_string(),
                    refresh_token: "new-refresh".to_string(),
                    email: "bob@example.com".to_string(),
                    expires_at: "2030-01-01T00:00:00Z".to_string(),
                    account_uuid: "acct-codex".to_string(),
                    provider: "codex".parse().unwrap(),
                    id_token: None,
                    last_refresh_at: None,
                    plan_type: None,
                })
            })
        },
        RefreshPolicy {
            kind: RefreshPolicyKind::SinceLastRefresh,
            seconds: 8 * 24 * 60 * 60,
        },
    );
    manager.load().expect("load accounts");

    assert!(
        manager
            .refresh_if_due("bob@example.com")
            .await
            .expect("refresh")
    );
    assert_eq!(*refresh_calls.lock().unwrap(), ["old-refresh"]);

    let snapshots = manager.snapshots();
    assert!(snapshots[0]["lastRefreshAt"].is_string());
    assert_eq!(snapshots[0]["email"], "bob@example.com");
}

#[tokio::test]
async fn exhausted_refresh_token_marks_account_for_reauth() {
    let tmp = tempdir().expect("tempdir");
    fs::write(
        tmp.path().join("codex-bob_example_com.json"),
        json!({
            "access_token": "old-access",
            "refresh_token": "old-refresh",
            "email": "bob@example.com",
            "type": "codex",
            "expired": "2000-01-01T00:00:00Z",
            "account_uuid": "acct-codex"
        })
        .to_string(),
    )
    .expect("write token");
    let mut manager = AccountManager::new(
        tmp.path().to_path_buf(),
        "codex".parse().unwrap(),
        |_refresh_token| {
            Box::pin(async move {
                Err(RefreshTokenExhaustedError::new(
                    "invalid_grant",
                    Some(400),
                    Some("invalid_grant".to_string()),
                )
                .into())
            })
        },
        RefreshPolicy::default(),
    );
    manager.load().expect("load accounts");

    assert!(
        !manager
            .refresh_if_due("bob@example.com")
            .await
            .expect("refresh result")
    );

    let snapshots = manager.snapshots();
    assert_eq!(snapshots[0]["available"], false);
    assert_eq!(snapshots[0]["failureCount"], 1);
    assert_eq!(snapshots[0]["totalFailures"], 1);
    assert_eq!(
        snapshots[0]["lastError"],
        "refresh token invalid_grant; re-run login for codex"
    );
}
