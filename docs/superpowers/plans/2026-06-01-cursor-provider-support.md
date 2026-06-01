# Cursor Provider Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Cursor as a fourth upstream provider focused on Cursor's own `composer-2.5` model, reachable via the `cursor/` model prefix, with browser-login + local-import auth and a reverse-engineered Connect-RPC/protobuf chat path.

**Architecture:** Cursor is a "Codex-shaped" provider — its upstream module decodes Cursor's protobuf and emits OpenAI-Responses SSE/JSON, so all client-format fan-out (`responses_sse_to_chat`, `responses_to_anthropic_message`, …) is reused by grouping `Codex | Cursor` in the existing match arms. The reverse-engineered protocol (protobuf encode, `x-cursor-checksum`, HTTP/2 transport, frame decode) is confined to `src/cursor.rs`; auth (browser poll, local SQLite import, refresh) to `src/cursor_auth.rs`. Every change is additive — new enum variant, new optional field, new match arms, new modules — so existing providers and on-disk token files are untouched.

**Tech Stack:** Rust 2024, axum 0.8, reqwest 0.12 (rustls + http2), tokio, serde_json. New deps: `rusqlite` (bundled), `flate2`, `uuid` `v5` feature.

**Spec:** `docs/superpowers/specs/2026-06-01-cursor-provider-support-design.md`

**Reference port source:** auth2api (`src/auth/cursor/*`, `src/upstream/cursor-api.ts`). Field numbers and the `jyh` cipher must match it byte-for-byte.

---

## File Structure

- `src/types.rs` (modify) — `ProviderId::Cursor`; `CursorMeta` struct; `TokenData.cursor: Option<CursorMeta>`.
- `src/tokens.rs` (modify) — persist/load cursor meta; `"cursor-"` filename allowlist.
- `src/accounts.rs` (modify) — carry `cursor` meta through `refresh_account`.
- `src/providers.rs` (modify) — register Cursor provider; `cursor/` prefix routing; `CURSOR_MODELS`.
- `src/cursor_auth.rs` (create) — browser deep-control login, local SQLite import, token refresh, constants.
- `src/cursor.rs` (create) — protobuf encode (incl. system-fold), checksum/headers, HTTP/2 `send_bytes`, frame decode, Responses SSE/JSON adaptation.
- `src/app.rs` (modify) — `AccountManagers.cursor`; trait methods; `route_cursor_request`; `Codex | Cursor` match arms; `/v1/models`; admin; lookup arms; real upstream impl.
- `src/cli.rs` (modify) — `cursor` in `--provider`; `--cursor-import-local` flag.
- `src/runtime.rs` (modify) — cursor login branch (poll / import).
- `src/lib.rs` (modify) — declare `mod cursor; mod cursor_auth;`.
- `Cargo.toml` (modify) — `rusqlite`, `flate2`, `uuid` `v5`.

Existing `TokenData { … }` literals that must gain a `cursor:` field (compiler-enforced): `src/oauth.rs:213,256`, `src/accounts.rs:216`, `src/tokens.rs:132`, `src/runtime.rs:269`, `src/app.rs:1923,2096`, and in tests `tests/app.rs` (15 sites), `tests/upstream.rs:41`, `tests/accounts.rs:35`, `tests/config_tokens_providers.rs:113,128,143`.

---

# Phase 1 — Scaffolding

Goal: `ProviderId::Cursor` exists end-to-end, `cursor/` routes to a (stub) Cursor manager, `/v1/models` lists `cursor/composer-2.5`, build + all existing tests pass. No real Cursor calls yet.

## Task 1: Add `ProviderId::Cursor`

**Files:**
- Modify: `src/types.rs:8-46`
- Test: `src/types.rs` (existing `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test** — add to `src/types.rs` tests module:

```rust
#[test]
fn cursor_provider_id_parses_and_displays() {
    assert_eq!("cursor".parse::<ProviderId>(), Ok(ProviderId::Cursor));
    assert_eq!(ProviderId::Cursor.to_string(), "cursor");
    assert_eq!(ProviderId::Cursor.storage_prefix(), "cursor");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib types::tests::cursor_provider_id_parses_and_displays`
Expected: FAIL — `no variant named Cursor`.

- [ ] **Step 3: Add the variant and its three impls**

In `src/types.rs` add `Cursor,` to the `ProviderId` enum (after `OpenCodeGo`). Then:

```rust
// in storage_prefix()
Self::Cursor => "cursor",
// in Display
Self::Cursor => formatter.write_str("cursor"),
// in FromStr match
"cursor" => Ok(Self::Cursor),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib types::tests::cursor_provider_id_parses_and_displays`
Expected: PASS. (`cargo build` will now fail elsewhere on non-exhaustive matches — fixed in later tasks. That is expected.)

- [ ] **Step 5: Commit**

```bash
git add src/types.rs
git commit -m "add cursor provider id variant"
```

## Task 2: Add `CursorMeta` and `TokenData.cursor`

**Files:**
- Modify: `src/types.rs` (after `RefreshTokenExhaustedError` or near `TokenData`)
- Modify (compiler-enforced `cursor:` additions): `src/oauth.rs`, `src/accounts.rs`, `src/tokens.rs`, `src/runtime.rs`, `src/app.rs` tests, `tests/*`
- Test: `src/types.rs` tests module

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn token_data_defaults_cursor_none() {
    let meta = super::CursorMeta {
        service_machine_id: Some("m".into()),
        client_version: "cli-x".into(),
        config_version: "cfg".into(),
        client_id: "cid".into(),
    };
    assert_eq!(meta.client_id, "cid");
    assert_eq!(meta.service_machine_id.as_deref(), Some("m"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib types::tests::token_data_defaults_cursor_none`
Expected: FAIL — `CursorMeta` not found.

- [ ] **Step 3: Define `CursorMeta` and add the field**

In `src/types.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorMeta {
    pub service_machine_id: Option<String>,
    pub client_version: String,
    pub config_version: String,
    pub client_id: String,
}
```

Add to `TokenData` (after `plan_type`):

```rust
    pub cursor: Option<CursorMeta>,
```

- [ ] **Step 4: Fix every `TokenData` literal the compiler now rejects**

Run `cargo build` and add the field to each literal it flags:
- `src/oauth.rs:213` (anthropic_token), `src/oauth.rs:256` (codex_token), `src/runtime.rs:269` (opencode-go), `src/tokens.rs:132` (storage_to_token), and all `tests/*` + `src/app.rs` test literals: add `cursor: None,`.
- `src/accounts.rs:216` (`new_token` in `refresh_account`): add `cursor: refreshed.cursor.or(old_token.cursor),`.

- [ ] **Step 5: Run build + the test**

Run: `cargo build && cargo test --lib types::tests::token_data_defaults_cursor_none`
Expected: build OK (matches still non-exhaustive only where Task 4/6 will add arms — if build still fails it will be on `match provider` arms, which is fine to leave until those tasks; if so, run `cargo test --lib types::` after Task 6). At minimum the literal edits compile.

- [ ] **Step 6: Commit**

```bash
git add src/types.rs src/oauth.rs src/accounts.rs src/tokens.rs src/runtime.rs src/app.rs tests/
git commit -m "add cursor metadata to token data"
```

## Task 3: Persist and load cursor token metadata

**Files:**
- Modify: `src/tokens.rs:10-22` (StoredToken), `:55-102` (load allowlist), `:104-143` (round-trip)
- Test: `src/tokens.rs` tests module (create one if absent)

- [ ] **Step 1: Write the failing test**

```rust
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
            last_refresh_at: None,
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib tokens::cursor_tests::cursor_token_round_trips_with_meta`
Expected: FAIL — cursor fields not persisted, loaded token has `cursor: None`.

- [ ] **Step 3: Extend `StoredToken` and the round-trip**

Add to `StoredToken` (each optional, skip when none):

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor_service_machine_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor_client_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor_config_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor_client_id: Option<String>,
```

In `token_to_storage`, set `token_type` to `"cursor"` for `ProviderId::Cursor` and populate the four fields from `token.cursor`:

```rust
        token_type: Some(match token.provider {
            ProviderId::Anthropic => "claude".to_string(),
            ProviderId::Codex => "codex".to_string(),
            ProviderId::OpenCodeGo => "opencodego".to_string(),
            ProviderId::Cursor => "cursor".to_string(),
        }),
        // ...after existing fields:
        cursor_service_machine_id: token.cursor.as_ref().and_then(|c| c.service_machine_id.clone()),
        cursor_client_version: token.cursor.as_ref().map(|c| c.client_version.clone()),
        cursor_config_version: token.cursor.as_ref().map(|c| c.config_version.clone()),
        cursor_client_id: token.cursor.as_ref().map(|c| c.client_id.clone()),
```

In `storage_to_token`, add `Some("cursor") => ProviderId::Cursor` to the provider match and rebuild `cursor`:

```rust
        cursor: (provider == ProviderId::Cursor).then(|| crate::types::CursorMeta {
            service_machine_id: stored.cursor_service_machine_id,
            client_version: stored.cursor_client_version.unwrap_or_default(),
            config_version: stored.cursor_config_version.unwrap_or_default(),
            client_id: stored.cursor_client_id.unwrap_or_default(),
        }),
```

In `load_all_tokens`, extend the `provider.is_none()` filename allowlist to also accept `filename.starts_with("cursor-")`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib tokens::cursor_tests::cursor_token_round_trips_with_meta`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/tokens.rs
git commit -m "persist cursor token metadata"
```

## Task 4: Register Cursor provider + prefix routing

**Files:**
- Modify: `src/providers.rs`
- Test: `src/providers.rs` tests module

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn routes_cursor_prefix() {
    let registry = build_registry(Path::new("/tmp"));
    assert_eq!(registry.for_model("cursor/composer-2.5").id, ProviderId::Cursor);
    assert_eq!(registry.for_model("cursor/anything").id, ProviderId::Cursor);
    // bare names never hijack other providers
    assert_eq!(registry.for_model("composer-2.5").id, ProviderId::Anthropic);
    assert_eq!(strip_cursor_prefix("cursor/composer-2.5"), "composer-2.5");
    assert_eq!(strip_cursor_prefix("composer-2.5"), "composer-2.5");
}
```

Add `strip_cursor_prefix` to the test's `use super::{...}` import.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib providers::tests::routes_cursor_prefix`
Expected: FAIL — `strip_cursor_prefix` / Cursor routing missing.

- [ ] **Step 3: Implement routing**

In `src/providers.rs`:

```rust
pub const CURSOR_PREFIX: &str = "cursor/";

/// Cursor-native models advertised on `/v1/models`. Routing accepts any `cursor/<sku>`.
pub const CURSOR_MODELS: [&str; 1] = ["composer-2.5"];

#[must_use]
pub fn strip_cursor_prefix(model: &str) -> &str {
    model.strip_prefix(CURSOR_PREFIX).unwrap_or(model)
}

fn cursor_matches_model(model: &str) -> bool {
    model.starts_with(CURSOR_PREFIX)
}
```

In `ProviderRegistry::get`, add the fallback arm:

```rust
            ProviderId::Cursor => Provider {
                id: ProviderId::Cursor,
                native_format: "openai-responses",
            },
```

In `for_model`, add the cursor check first (before opencode-go is fine; prefixes are disjoint):

```rust
        if cursor_matches_model(&resolved) {
            return self.get(ProviderId::Cursor);
        }
```

In `build_registry`, push:

```rust
            Provider { id: ProviderId::Cursor, native_format: "openai-responses" },
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib providers::tests::routes_cursor_prefix`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/providers.rs
git commit -m "route cursor model prefix"
```

## Task 5: Wire the Cursor account manager + stub upstream + match arms

**Files:**
- Modify: `src/app.rs` — imports, `AccountManagers`, `UpstreamClient` trait, `HttpUpstreamClient` impl (stub), `build_account_managers`, `route_provider_request`, `route_cursor_request`, `provider_account_count`, account-manager lookup arms, `json_upstream_response`, `transform_sse_event`, `update_stream_usage`, `count_tokens` guard, `/v1/models`, `admin_accounts`, `admin_reload`, test fakes.
- Modify: `src/lib.rs` — `mod cursor;` is added in Phase 3; for now no new module.
- Test: `tests/app.rs` (integration)

- [ ] **Step 1: Write the failing test** — add to `tests/app.rs`:

```rust
#[tokio::test]
async fn cursor_models_listed_when_account_present() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(tmp.path(), &cursor_token()).expect("save cursor token");
    let state = cursor_state(tmp.path());
    let response = models(axum::extract::State(state), auth_header()).await;
    let body = response_json(response).await;
    let ids: Vec<&str> = body["data"].as_array().unwrap()
        .iter().filter_map(|m| m["id"].as_str()).collect();
    assert!(ids.contains(&"cursor/composer-2.5"), "{ids:?}");
}
```

Add a `cursor_token()` builder and `cursor_state()` helper mirroring `opencode_go_token()`/`opencode_go_state()` (set `provider: ProviderId::Cursor`, `cursor: Some(CursorMeta { service_machine_id: Some("m".into()), client_version: "cli-x".into(), config_version: "cfg".into(), client_id: "cid".into() })`). Reuse existing `auth_header`/`response_json` test helpers (copy their names from the surrounding tests).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test app cursor_models_listed_when_account_present`
Expected: FAIL — `cursor` field missing on `AccountManagers`, no Cursor arms.

- [ ] **Step 3: Add the manager, trait methods, stub impl, and arms**

`AccountManagers`:

```rust
    cursor: tokio::sync::Mutex<AccountManager>,
```

`UpstreamClient` trait — add:

```rust
    fn cursor_responses(&self, request: UpstreamRequest) -> UpstreamFuture;
    fn cursor_responses_stream(&self, request: UpstreamRequest) -> UpstreamSseFuture;
```

`HttpUpstreamClient` stub impl (replaced in Phase 3):

```rust
    fn cursor_responses(&self, _request: UpstreamRequest) -> UpstreamFuture {
        Box::pin(async { anyhow::bail!("cursor provider not yet implemented") })
    }
    fn cursor_responses_stream(&self, _request: UpstreamRequest) -> UpstreamSseFuture {
        Box::pin(async { anyhow::bail!("cursor provider not yet implemented") })
    }
```

`build_account_managers` — construct + load + include + log:

```rust
    let mut cursor = AccountManager::new(
        config.auth_dir.clone(),
        ProviderId::Cursor,
        |refresh_token| Box::pin(crate::cursor_auth::refresh_cursor_tokens(refresh_token)),
        RefreshPolicy { kind: RefreshPolicyKind::ExpiresLead, seconds: 600 },
    );
    let _ = cursor.load();
    // add `cursor = cursor.account_count()` to the tracing::info! fields
    // add `cursor: tokio::sync::Mutex::new(cursor),` to the returned struct
```

> Note: `crate::cursor_auth::refresh_cursor_tokens` is created in Phase 2. To keep Phase 1 compiling on its own, temporarily use a placeholder closure `|_refresh_token| Box::pin(async { anyhow::bail!("cursor refresh not yet implemented") }) as RefreshFuture` and switch to the real fn in Phase 2 Task 9 Step 3.

`route_provider_request` dispatch — add:

```rust
            ProviderId::Cursor => {
                route_cursor_request(state, headers, body, route, &model, &account, client_wants_stream).await
            }
```

`route_cursor_request` — clone of `route_codex_request`, using `codex_request_body` and the cursor upstream methods:

```rust
async fn route_cursor_request(
    state: &AppState,
    headers: &HeaderMap,
    body: &Value,
    route: RequestRoute,
    model: &str,
    account: &AvailableAccount,
    client_wants_stream: bool,
) -> Response {
    let body = codex_request_body(body, model, route);
    if client_wants_stream {
        return match state.upstream.cursor_responses_stream(UpstreamRequest {
            body, request_headers: headers_to_map(headers), account: account.clone(), config: (*state.config).clone(),
        }).await {
            Ok(response) => {
                let accounting = stream_accounting(state, ProviderId::Cursor, account, response.status).await;
                sse_upstream_response(response, ProviderId::Cursor, route, model, accounting)
            }
            Err(error) => upstream_failure_response(state, ProviderId::Cursor, account, &error).await,
        };
    }
    match state.upstream.cursor_responses(UpstreamRequest {
        body, request_headers: headers_to_map(headers), account: account.clone(), config: (*state.config).clone(),
    }).await {
        Ok(response) => {
            record_json_result(state, ProviderId::Cursor, account, &response).await;
            json_upstream_response(response, ProviderId::Cursor, route, model)
        }
        Err(error) => upstream_failure_response(state, ProviderId::Cursor, account, &error).await,
    }
}
```

Add `ProviderId::Cursor => state.account_managers.cursor.lock().await...` to `provider_account_count` and to the three account-manager lookup match sites (the ones at the former lines `1098`, `1147`, `1162`).

Group Cursor with Codex in the response reshaping:
- `json_upstream_response`: change `(ProviderId::Codex, RequestRoute::Responses)` arms to `(ProviderId::Codex | ProviderId::Cursor, RequestRoute::Responses)`, and likewise the Chat (`responses_to_chat_completion`) and Messages (`responses_to_anthropic_message`) arms.
- `transform_sse_event`: same `Codex | Cursor` grouping in the `[DONE]` arm and the Responses/Chat/Messages arms.
- `update_stream_usage`: `ProviderId::Codex | ProviderId::Cursor => update_codex_stream_usage(...)`.
- `count_tokens` guard: `matches!(provider.id, ProviderId::Codex | ProviderId::OpenCodeGo | ProviderId::Cursor)`.

`/v1/models`: add `ProviderId::Cursor => false` to the seed-list match, and after the opencode-go block append:

```rust
    let has_cursor = state.account_managers.cursor.lock().await.account_count() > 0;
    if has_cursor {
        models.extend(crate::providers::CURSOR_MODELS.iter().map(|id| json!({
            "id": format!("cursor/{id}"),
            "object": "model",
            "owned_by": ProviderId::Cursor.to_string()
        })));
    }
```

`admin_accounts` + `admin_reload`: add a `cursor` lock + entry mirroring `opencode_go` (include in the `match (anthropic, codex, opencode_go, cursor)` reload tuple).

Test fakes: every `impl UpstreamClient for <Fake>` in `tests/app.rs` and `src/app.rs` tests must add `cursor_responses`/`cursor_responses_stream` (delegate to the existing fake behavior or `unreachable!()` where unused). Add a `cursor_state()` test helper that builds `AccountManagers` with a real `cursor` manager.

- [ ] **Step 4: Run the test + full suite**

Run: `cargo test --test app cursor_models_listed_when_account_present && cargo test`
Expected: PASS; all existing tests still green.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs tests/app.rs
git commit -m "wire cursor account manager and stub upstream"
```

## Task 6: Phase 1 verification

- [ ] **Step 1: Build, lint, test**

Run: `cargo build && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: clean build, no clippy errors, all tests pass.

- [ ] **Step 2: Commit any clippy fixes**

```bash
git add -A
git commit -m "satisfy clippy for cursor scaffolding"
```

---

# Phase 2 — Auth

Goal: `pengepul login --provider cursor` (browser poll) and `--cursor-import-local` (SQLite) both work and persist a `cursor-*.json` token; refresh works. Add deps.

## Task 7: Add dependencies + module declarations

**Files:**
- Modify: `Cargo.toml`, `src/lib.rs`

- [ ] **Step 1: Add deps to `Cargo.toml`**

```toml
flate2 = "1"
rusqlite = { version = "0.32", features = ["bundled"] }
```

Change the `uuid` line to add `v5`:

```toml
uuid = { version = "1", features = ["v4", "v5"] }
```

- [ ] **Step 2: Declare modules in `src/lib.rs`**

```rust
pub mod cursor;
pub mod cursor_auth;
```

(Create empty `src/cursor.rs` and `src/cursor_auth.rs` with a `// filled in by later tasks` comment so the crate compiles.)

- [ ] **Step 3: Verify it builds**

Run: `cargo build`
Expected: PASS (downloads rusqlite/flate2, compiles bundled SQLite).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/cursor.rs src/cursor_auth.rs
git commit -m "add cursor dependencies and module stubs"
```

## Task 8: Cursor constants + token refresh

**Files:**
- Modify: `src/cursor_auth.rs`
- Test: `src/cursor_auth.rs` tests module

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_request_is_json_with_refresh_grant() {
        let req = cursor_refresh_request("rt-123", CURSOR_CLIENT_ID).build().expect("builds");
        assert_eq!(req.url().as_str(), CURSOR_TOKEN_URL);
        let ct = req.headers().get(reqwest::header::CONTENT_TYPE).unwrap().to_str().unwrap();
        assert_eq!(ct, "application/json");
        let body = std::str::from_utf8(req.body().unwrap().as_bytes().unwrap()).unwrap();
        assert!(body.contains("\"grant_type\":\"refresh_token\""), "{body}");
        assert!(body.contains("\"refresh_token\":\"rt-123\""), "{body}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib cursor_auth::tests::refresh_request_is_json_with_refresh_grant`
Expected: FAIL — items not defined.

- [ ] **Step 3: Implement constants + refresh**

```rust
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
            "invalidated", None,
            Some("no refresh token stored; re-run login --provider cursor".into()),
        ).into());
    }
    let response = cursor_refresh_request(&refresh_token, CURSOR_CLIENT_ID).send().await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        if let Some(reason) = crate::oauth::detect_exhausted_reason(&body) {
            return Err(RefreshTokenExhaustedError::new(reason, Some(status.as_u16()), Some(body)).into());
        }
        bail!("cursor token refresh failed ({status}): {body}");
    }
    let data: Value = serde_json::from_str(&body).context("Cursor refresh response is not JSON")?;
    if data.get("shouldLogout").and_then(Value::as_bool).unwrap_or(false) {
        return Err(RefreshTokenExhaustedError::new("invalidated", Some(status.as_u16()), Some(body)).into());
    }
    let access_token = data.get("access_token").and_then(Value::as_str)
        .context("cursor refresh response missing access_token")?.to_string();
    let new_refresh = data.get("refresh_token").and_then(Value::as_str)
        .map_or(refresh_token, ToOwned::to_owned);
    Ok(TokenData {
        access_token: access_token.clone(),
        refresh_token: new_refresh,
        email: String::new(), // preserved by AccountManager from the old token
        expires_at: expiry_from_jwt(&access_token),
        account_uuid: String::new(), // preserved by AccountManager
        provider: ProviderId::Cursor,
        id_token: data.get("id_token").and_then(Value::as_str).map(ToOwned::to_owned),
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
```

> Note: `AccountManager::refresh_account` already preserves `email`/`account_uuid` when the refreshed token leaves them empty, and (after Phase 1 Task 2) carries `cursor` forward with `refreshed.cursor.or(old_token.cursor)` — so the stored `service_machine_id` survives refresh.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib cursor_auth::tests::refresh_request_is_json_with_refresh_grant`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cursor_auth.rs
git commit -m "implement cursor token refresh"
```

## Task 9: Local desktop import (SQLite)

**Files:**
- Modify: `src/cursor_auth.rs`
- Test: `src/cursor_auth.rs` tests module

- [ ] **Step 1: Write the failing test**

```rust
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
    assert_eq!(token.cursor.unwrap().service_machine_id.as_deref(), Some("machine-1"));
}

#[test]
fn token_from_storage_requires_tokens() {
    use std::collections::BTreeMap;
    assert!(cursor_token_from_storage(&BTreeMap::new()).is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib cursor_auth::tests::token_from_storage`
Expected: FAIL — `cursor_token_from_storage` not defined.

- [ ] **Step 3: Implement storage read + token build**

```rust
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
    let conn = rusqlite::Connection::open_with_flags(
        path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ).with_context(|| format!("failed to open cursor storage {}", path.display()))?;
    let mut out = BTreeMap::new();
    for table in ["ItemTable", "cursorDiskKV"] {
        let sql = format!("SELECT key, value FROM {table} WHERE key IN ({})",
            CURSOR_KEYS.iter().map(|_| "?").collect::<Vec<_>>().join(","));
        let Ok(mut stmt) = conn.prepare(&sql) else { continue };
        let rows = stmt.query_map(rusqlite::params_from_iter(CURSOR_KEYS.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        });
        if let Ok(rows) = rows {
            for row in rows.flatten() { out.insert(row.0, row.1); }
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
    let access_token = storage.get("cursorAuth/accessToken").map(|v| coerce(v))
        .filter(|v| !v.is_empty()).context("cursor storage missing accessToken")?;
    let refresh_token = storage.get("cursorAuth/refreshToken").map(|v| coerce(v))
        .filter(|v| !v.is_empty()).context("cursor storage missing refreshToken")?;
    let machine_id = storage.get("storage.serviceMachineId").map(|v| coerce(v));
    Ok(TokenData {
        access_token: access_token.clone(),
        refresh_token,
        email: storage.get("cursorAuth/cachedEmail").map(|v| coerce(v)).unwrap_or_else(|| "unknown".into()),
        expires_at: expiry_from_jwt(&access_token),
        account_uuid: machine_id.clone().unwrap_or_default(),
        provider: ProviderId::Cursor,
        id_token: None,
        last_refresh_at: None,
        plan_type: storage.get("cursorAuth/stripeMembershipType").map(|v| coerce(v)),
        cursor: Some(CursorMeta {
            service_machine_id: machine_id,
            client_version: storage.get("cursorAuth/clientVersion").map(|v| coerce(v))
                .unwrap_or_else(|| CURSOR_DEFAULT_CLIENT_VERSION.into()),
            config_version: storage.get("cursorAuth/configVersion").map(|v| coerce(v)).unwrap_or_default(),
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
```

- [ ] **Step 4: Add a SQLite round-trip test**

```rust
#[test]
fn import_reads_real_sqlite() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("state.vscdb");
    let conn = rusqlite::Connection::open(&db).expect("open");
    conn.execute("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)", []).expect("create");
    conn.execute("INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
        rusqlite::params!["cursorAuth/accessToken", "jwt"]).expect("ins1");
    conn.execute("INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
        rusqlite::params!["cursorAuth/refreshToken", "rt"]).expect("ins2");
    drop(conn);
    let token = import_cursor_local(&db).expect("import");
    assert_eq!(token.refresh_token, "rt");
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib cursor_auth::tests`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/cursor_auth.rs
git commit -m "import cursor token from local sqlite storage"
```

## Task 10: Browser deep-control login (PKCE + poll)

**Files:**
- Modify: `src/cursor_auth.rs`
- Test: `src/cursor_auth.rs` tests module

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn login_url_has_challenge_and_cli_target() {
    let pkce = CursorPkce { uuid: "u1".into(), verifier: "v".into(), challenge: "c1".into() };
    let url = build_cursor_login_url(&pkce);
    assert!(url.contains("challenge=c1"), "{url}");
    assert!(url.contains("uuid=u1"), "{url}");
    assert!(url.contains("redirectTarget=cli"), "{url}");
}

#[test]
fn poll_response_without_refresh_token_is_error() {
    let pkce = CursorPkce { uuid: "u1".into(), verifier: "v".into(), challenge: "c1".into() };
    let result = poll_result_to_token(
        &serde_json::json!({"accessToken": "jwt", "authId": "auth0|user_x"}), &pkce);
    assert!(result.is_err());
}

#[test]
fn poll_response_with_refresh_token_builds_token() {
    let pkce = CursorPkce { uuid: "u1".into(), verifier: "v".into(), challenge: "c1".into() };
    let token = poll_result_to_token(
        &serde_json::json!({"accessToken": "jwt", "refreshToken": "rt", "authId": "auth0|user_x"}), &pkce
    ).expect("token");
    assert_eq!(token.refresh_token, "rt");
    assert_eq!(token.account_uuid, "u1");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib cursor_auth::tests::login_url_has_challenge_and_cli_target`
Expected: FAIL — items not defined.

- [ ] **Step 3: Implement PKCE, URL, poll, and poll→token**

```rust
use sha2::{Digest, Sha256};

pub const CURSOR_LOGIN_URL: &str = "https://www.cursor.com/loginDeepControl";
pub const CURSOR_POLL_URL: &str = "https://api2.cursor.sh/auth/poll";

pub struct CursorPkce { pub uuid: String, pub verifier: String, pub challenge: String }

fn base64_url(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[must_use]
pub fn generate_cursor_pkce() -> CursorPkce {
    use rand::RngCore as _;
    let mut verifier_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut verifier_bytes);
    let verifier = base64_url(&verifier_bytes);
    let challenge = base64_url(&Sha256::digest(verifier.as_bytes()));
    CursorPkce { uuid: uuid::Uuid::new_v4().to_string(), verifier, challenge }
}

#[must_use]
pub fn build_cursor_login_url(pkce: &CursorPkce) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("challenge", &pkce.challenge)
        .append_pair("uuid", &pkce.uuid)
        .append_pair("mode", "login")
        .append_pair("redirectTarget", "cli")
        .finish();
    format!("{CURSOR_LOGIN_URL}?{query}")
}

fn email_from_auth_id(auth_id: Option<&str>) -> String {
    match auth_id {
        Some(id) => {
            let tail = id.rsplit('|').next().unwrap_or(id);
            let safe: String = tail.chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
                .collect();
            format!("{safe}@cursor.local")
        }
        None => "unknown@cursor.local".to_string(),
    }
}

/// Convert a successful `auth/poll` payload into a stored token.
///
/// # Errors
/// Returns an error when no refresh token is present (PAT-mode session) — persisting the access
/// token as a refresh credential would push the account into permanent auth failure.
pub fn poll_result_to_token(payload: &Value, pkce: &CursorPkce) -> Result<TokenData> {
    let access_token = payload.get("accessToken").and_then(Value::as_str)
        .or_else(|| payload.get("apiKey").and_then(Value::as_str))
        .context("cursor poll result missing accessToken")?.to_string();
    let refresh_token = payload.get("refreshToken").and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .context("cursor browser login returned no refresh token; re-run login or use --cursor-import-local")?
        .to_string();
    Ok(TokenData {
        access_token: access_token.clone(),
        refresh_token,
        email: email_from_auth_id(payload.get("authId").and_then(Value::as_str)),
        expires_at: expiry_from_jwt(&access_token),
        account_uuid: pkce.uuid.clone(),
        provider: ProviderId::Cursor,
        id_token: None,
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

/// Poll `auth/poll` until the user confirms in the browser (or the deadline passes).
///
/// # Errors
/// Returns an error on timeout or a non-pending HTTP failure.
pub async fn poll_cursor_auth(pkce: &CursorPkce) -> Result<TokenData> {
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    let mut interval = std::time::Duration::from_millis(1000);
    loop {
        if std::time::Instant::now() > deadline {
            bail!("cursor browser login timed out before the user confirmed");
        }
        let url = url::form_urlencoded::Serializer::new(format!("{CURSOR_POLL_URL}?"))
            .append_pair("uuid", &pkce.uuid)
            .append_pair("verifier", &pkce.verifier)
            .finish();
        match client.get(&url).header("Accept", "application/json").send().await {
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                if status.is_success()
                    && let Ok(payload) = serde_json::from_str::<Value>(&body)
                    && (payload.get("accessToken").is_some() || payload.get("apiKey").is_some())
                {
                    return poll_result_to_token(&payload, pkce);
                }
                // 404/202/empty => still pending
            }
            Err(_) => { /* transient; keep polling */ }
        }
        tokio::time::sleep(interval).await;
        interval = (interval * 6 / 5).min(std::time::Duration::from_secs(5));
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib cursor_auth::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cursor_auth.rs
git commit -m "implement cursor browser deep-control login"
```

## Task 11: CLI + runtime login wiring

**Files:**
- Modify: `src/cli.rs:142-152,463-482`, `src/runtime.rs:127-167`
- Test: `tests/cli.rs` (mirror an existing login test)

- [ ] **Step 1: Write the failing test** — add to `tests/cli.rs` a test asserting `--provider cursor --cursor-import-local` routes through a fake `CliRuntime` that records `(provider, import_local)`; assert it receives `(ProviderId::Cursor, true)`. (Copy the structure of the nearest existing login CLI test; extend the fake `CliRuntime::login` signature.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test cli cursor`
Expected: FAIL — `--cursor-import-local` unknown; `login` signature mismatch.

- [ ] **Step 3: Wire CLI + runtime**

`src/cli.rs` — `Login` command:

```rust
        #[arg(long, default_value = "anthropic", value_parser = ["anthropic", "codex", "opencode-go", "cursor"])]
        provider: String,
        // ...existing manual/key...
        /// import the locally installed Cursor desktop login instead of the browser flow
        #[arg(long = "cursor-import-local")]
        cursor_import_local: bool,
```

`CliRuntime::login` trait signature — add `import_local: bool`. Update the `login()` free fn to pass it, and guard:

```rust
    if provider != ProviderId::Cursor && import_local {
        bail!("--cursor-import-local is only valid with --provider cursor");
    }
    if provider == ProviderId::Cursor && manual {
        bail!("--manual is not supported for cursor");
    }
    let email = runtime.login(&config, provider, manual, key, import_local)?;
```

Pass `cursor_import_local` from the `Command::Login` match arm into `login(...)`.

`src/runtime.rs` `RealRuntime::login` — add `import_local: bool` param and branch before the OAuth path:

```rust
        if provider == ProviderId::Cursor {
            let token = if import_local {
                let home = home_dir()?;
                crate::cursor_auth::import_cursor_local(&crate::cursor_auth::default_cursor_storage_path(&home))?
            } else {
                let pkce = crate::cursor_auth::generate_cursor_pkce();
                let url = crate::cursor_auth::build_cursor_login_url(&pkce);
                println!("\nOpen this URL to authorize cursor:\n\n{url}\n");
                if !manual { open_browser(&url); }
                self.runtime.block_on(crate::cursor_auth::poll_cursor_auth(&pkce))?
            };
            let email = token.email.clone();
            save_token(&config.auth_dir, &token)?;
            return Ok(email);
        }
        if provider == ProviderId::OpenCodeGo {
            return save_opencode_go_login(config, key);
        }
```

Update the `match provider` arms in `auth_url`/`callback_endpoint` to add `ProviderId::Cursor => unreachable!("cursor login handled before the OAuth flow")`.

Switch the Phase 1 placeholder refresh closure in `build_account_managers` to the real `crate::cursor_auth::refresh_cursor_tokens`.

- [ ] **Step 4: Run test + suite**

Run: `cargo test --test cli cursor && cargo build && cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cli.rs src/runtime.rs src/app.rs
git commit -m "wire cursor login command"
```

---

# Phase 3 — Protocol

Goal: real `cursor.rs` — encode the chat request (folding system into the first user turn), build headers + checksum, POST over HTTP/2, decode frames, emit Responses SSE / synth Responses JSON. Replace the Phase 1 stub.

## Task 12: Protobuf primitives (encode + decode)

**Files:**
- Modify: `src/cursor.rs`
- Test: `src/cursor.rs` tests module

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_and_field_encoding_match_known_vectors() {
        let mut v = Vec::new(); encode_varint(300, &mut v);
        assert_eq!(v, vec![0xac, 0x02]);
        let mut b = Vec::new(); encode_bytes_field(1, b"hi", &mut b);
        assert_eq!(b, vec![0x0a, 0x02, b'h', b'i']);
        let mut n = Vec::new(); encode_varint_field(2, 1, &mut n);
        assert_eq!(n, vec![0x10, 0x01]);
    }

    #[test]
    fn parse_fields_round_trips_a_bytes_field() {
        let mut buf = Vec::new();
        encode_bytes_field(1, b"hello", &mut buf);
        encode_varint_field(2, 7, &mut buf);
        let fields = parse_fields(&buf);
        assert_eq!(field_bytes(&fields, 1).unwrap(), b"hello");
        assert_eq!(fields.iter().find(|f| f.field == 2).unwrap().varint, Some(7));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib cursor::tests::varint_and_field_encoding_match_known_vectors`
Expected: FAIL — items not defined.

- [ ] **Step 3: Implement primitives**

```rust
pub(crate) fn encode_varint(mut value: u32, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

pub(crate) fn encode_varint_field(field: u32, value: u32, out: &mut Vec<u8>) {
    encode_varint(field << 3, out); // wire type 0
    encode_varint(value, out);
}

pub(crate) fn encode_bytes_field(field: u32, payload: &[u8], out: &mut Vec<u8>) {
    encode_varint((field << 3) | 2, out); // wire type 2
    encode_varint(u32::try_from(payload.len()).unwrap_or(u32::MAX), out);
    out.extend_from_slice(payload);
}

pub(crate) fn connect_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(0);
    frame.extend_from_slice(&u32::try_from(payload.len()).unwrap_or(u32::MAX).to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

#[derive(Debug)]
pub(crate) struct RawField { pub field: u32, pub wire: u8, pub bytes: Option<Vec<u8>>, pub varint: Option<u64> }

fn decode_varint(data: &[u8], pos: usize) -> (u64, usize) {
    let (mut value, mut shift, mut p) = (0u64, 0u32, pos);
    while p < data.len() {
        let b = data[p]; p += 1;
        value |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 { break; }
        shift += 7;
    }
    (value, p)
}

pub(crate) fn parse_fields(data: &[u8]) -> Vec<RawField> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let (tag, after) = decode_varint(data, pos);
        if after <= pos { break; }
        pos = after;
        let field = u32::try_from(tag >> 3).unwrap_or(0);
        let wire = (tag & 7) as u8;
        match wire {
            0 => { let (v, p) = decode_varint(data, pos); out.push(RawField { field, wire, bytes: None, varint: Some(v) }); pos = p; }
            2 => {
                let (len, after_len) = decode_varint(data, pos);
                pos = after_len;
                let len = len as usize;
                if pos + len > data.len() { break; }
                out.push(RawField { field, wire, bytes: Some(data[pos..pos + len].to_vec()), varint: None });
                pos += len;
            }
            1 => pos += 8,
            5 => pos += 4,
            _ => break,
        }
    }
    out
}

pub(crate) fn field_bytes(fields: &[RawField], field: u32) -> Option<&[u8]> {
    fields.iter().find(|f| f.field == field && f.wire == 2).and_then(|f| f.bytes.as_deref())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib cursor::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cursor.rs
git commit -m "add cursor protobuf primitives"
```

## Task 13: Message extraction + system fold + model normalization

**Files:**
- Modify: `src/cursor.rs`
- Test: `src/cursor.rs` tests module

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn folds_system_into_first_user_turn() {
    let body = serde_json::json!({
        "instructions": "be terse",
        "input": [{"role": "user", "content": "hi"}, {"role": "assistant", "content": "ok"}]
    });
    let msgs = messages_from_body(&body);
    assert_eq!(msgs[0].role, Role::User);
    assert_eq!(msgs[0].content, "be terse\n\nhi");
    assert_eq!(msgs[1].role, Role::Assistant);
    // no system role emitted
    assert!(msgs.iter().all(|m| m.role != Role::System));
}

#[test]
fn normalizes_model() {
    assert_eq!(normalize_model("cursor/composer-2.5"), "composer-2.5");
    assert_eq!(normalize_model("cursor/foo"), "foo");
    assert_eq!(normalize_model("cursor/"), "composer-2.5");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib cursor::tests::folds_system_into_first_user_turn`
Expected: FAIL — items not defined.

- [ ] **Step 3: Implement**

```rust
use serde_json::Value;
use crate::providers::strip_cursor_prefix;

pub(crate) const CURSOR_DEFAULT_MODEL: &str = "composer-2.5";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role { User, Assistant, System }

#[derive(Debug, Clone)]
pub(crate) struct ChatMessage { pub role: Role, pub content: String }

fn text_from_content(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter()
            .filter_map(|p| p.get("text").or_else(|| p.get("input_text")).and_then(Value::as_str))
            .collect::<Vec<_>>().join("\n"),
        Value::Object(_) => value.get("text").or_else(|| value.get("input_text"))
            .and_then(Value::as_str).unwrap_or_default().to_string(),
        _ => String::new(),
    }
}

#[must_use]
pub(crate) fn normalize_model(model: &str) -> String {
    let stripped = strip_cursor_prefix(model).trim();
    if stripped.is_empty() { CURSOR_DEFAULT_MODEL.to_string() } else { stripped.to_string() }
}

/// Extract chat turns from a Responses-shaped body. System text (top-level `system`,
/// `instructions`, or role:"system"/"developer" turns) is concatenated and prepended to the
/// first user turn — Cursor has no system role.
#[must_use]
pub(crate) fn messages_from_body(body: &Value) -> Vec<ChatMessage> {
    let mut system = Vec::new();
    let mut turns: Vec<ChatMessage> = Vec::new();
    let mut push = |role: Role, text: String| {
        if text.trim().is_empty() { return; }
        match role {
            Role::System => system.push(text),
            r => turns.push(ChatMessage { role: r, content: text }),
        }
    };
    if let Some(s) = body.get("system") { push(Role::System, text_from_content(s)); }
    if let Some(i) = body.get("instructions").filter(|v| !v.is_null()) { push(Role::System, text_from_content(i)); }
    match body.get("input") {
        Some(Value::String(s)) => push(Role::User, s.clone()),
        Some(Value::Array(items)) => for item in items {
            let role = match item.get("role").and_then(Value::as_str) {
                Some("assistant") => Role::Assistant,
                Some("system" | "developer") => Role::System,
                _ => Role::User,
            };
            push(role, text_from_content(item.get("content").unwrap_or(item)));
        },
        _ => {}
    }
    if let Some(Value::Array(items)) = body.get("messages") {
        for item in items {
            let role = match item.get("role").and_then(Value::as_str) {
                Some("assistant") => Role::Assistant,
                Some("system" | "developer") => Role::System,
                _ => Role::User,
            };
            push(role, text_from_content(item.get("content").unwrap_or(&Value::Null)));
        }
    }
    if !system.is_empty() {
        let prefix = system.join("\n\n");
        if let Some(first_user) = turns.iter_mut().find(|m| m.role == Role::User) {
            first_user.content = format!("{prefix}\n\n{}", first_user.content);
        } else {
            turns.insert(0, ChatMessage { role: Role::User, content: prefix });
        }
    }
    if turns.is_empty() { turns.push(ChatMessage { role: Role::User, content: String::new() }); }
    turns
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib cursor::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cursor.rs
git commit -m "extract chat messages and fold system prompt for cursor"
```

## Task 14: Encode the chat request

**Files:**
- Modify: `src/cursor.rs`
- Test: `src/cursor.rs` tests module

- [ ] **Step 1: Write the failing test** (round-trips via `parse_fields` — encode is non-deterministic due to uuids/timestamps, so assert structure, not bytes)

```rust
#[test]
fn encodes_chat_request_with_model_and_messages() {
    let body = serde_json::json!({"model": "cursor/composer-2.5", "input": "hello"});
    let frame = encode_cursor_chat_request(&body);
    // strip the 5-byte connect envelope
    assert_eq!(frame[0], 0);
    let len = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]) as usize;
    let payload = &frame[5..5 + len];
    // outer wrapper: field 1 = request message
    let outer = parse_fields(payload);
    let request = field_bytes(&outer, 1).expect("request field");
    let req_fields = parse_fields(request);
    // field 5 = model message, which contains field 1 = model name
    let model_msg = field_bytes(&req_fields, 5).expect("model field");
    let model_name = field_bytes(&parse_fields(model_msg), 1).expect("model name");
    assert_eq!(model_name, b"composer-2.5");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib cursor::tests::encodes_chat_request_with_model_and_messages`
Expected: FAIL — `encode_cursor_chat_request` not defined.

- [ ] **Step 3: Implement the encoder** (field numbers ported verbatim from auth2api `encodeCursorChatRequest`)

```rust
fn encode_chat_message(content: &str, role: u32, message_id: &str) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field(1, content.as_bytes(), &mut out);
    encode_varint_field(2, role, &mut out);
    encode_bytes_field(13, message_id.as_bytes(), &mut out);
    encode_varint_field(47, 2, &mut out); // chat mode enum
    out
}

fn encode_message_id(message_id: &str, role: u32) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field(1, message_id.as_bytes(), &mut out);
    encode_varint_field(3, role, &mut out);
    out
}

fn encode_model_msg(model: &str) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field(1, model.as_bytes(), &mut out);
    encode_bytes_field(4, &[], &mut out);
    out
}

fn encode_cursor_setting() -> Vec<u8> {
    let mut unknown6 = Vec::new();
    encode_bytes_field(1, &[], &mut unknown6);
    encode_bytes_field(2, &[], &mut unknown6);
    let mut out = Vec::new();
    encode_bytes_field(1, b"cursor\\aisettings", &mut out);
    encode_bytes_field(3, &[], &mut out);
    encode_bytes_field(6, &unknown6, &mut out);
    encode_varint_field(8, 1, &mut out);
    encode_varint_field(9, 1, &mut out);
    out
}

fn encode_metadata() -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field(1, std::env::consts::OS.as_bytes(), &mut out);
    encode_bytes_field(2, std::env::consts::ARCH.as_bytes(), &mut out);
    encode_bytes_field(3, env!("CARGO_PKG_VERSION").as_bytes(), &mut out);
    encode_bytes_field(4, b"pengepul", &mut out);
    encode_bytes_field(5, chrono::Utc::now().to_rfc3339().as_bytes(), &mut out);
    out
}

#[must_use]
pub(crate) fn encode_cursor_chat_request(body: &Value) -> Vec<u8> {
    let model = normalize_model(body.get("model").and_then(Value::as_str).unwrap_or("cursor/"));
    let messages = messages_from_body(body);
    let conversation_id = uuid::Uuid::new_v4().to_string();
    let mut entries: Vec<(String, u32)> = Vec::new();

    let mut request = Vec::new();
    for msg in &messages {
        let role = if msg.role == Role::Assistant { 2 } else { 1 };
        let id = uuid::Uuid::new_v4().to_string();
        entries.push((id.clone(), role));
        let message = encode_chat_message(&msg.content, role, &id);
        encode_bytes_field(1, &message, &mut request);
    }
    encode_varint_field(2, 1, &mut request);
    encode_bytes_field(3, &[], &mut request);
    encode_varint_field(4, 1, &mut request);
    let model_msg = encode_model_msg(&model);
    encode_bytes_field(5, &model_msg, &mut request);
    encode_bytes_field(8, b"", &mut request);
    encode_varint_field(13, 1, &mut request);
    let setting = encode_cursor_setting();
    encode_bytes_field(15, &setting, &mut request);
    encode_varint_field(19, 1, &mut request);
    encode_bytes_field(23, conversation_id.as_bytes(), &mut request);
    let metadata = encode_metadata();
    encode_bytes_field(26, &metadata, &mut request);
    encode_varint_field(27, 1, &mut request);
    for (id, role) in &entries {
        let msg_id = encode_message_id(id, *role);
        encode_bytes_field(30, &msg_id, &mut request);
    }
    encode_varint_field(35, 0, &mut request);
    encode_varint_field(38, 0, &mut request);
    encode_varint_field(46, 2, &mut request);
    encode_bytes_field(47, b"", &mut request);
    encode_varint_field(48, 0, &mut request);
    encode_varint_field(49, 0, &mut request);
    encode_varint_field(51, 0, &mut request);
    encode_varint_field(53, 1, &mut request);
    encode_bytes_field(54, b"agent", &mut request);

    let mut wrapped = Vec::new();
    encode_bytes_field(1, &request, &mut wrapped);
    connect_frame(&wrapped)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib cursor::tests::encodes_chat_request_with_model_and_messages`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cursor.rs
git commit -m "encode cursor chat request"
```

## Task 15: Checksum + request headers

**Files:**
- Modify: `src/cursor.rs`
- Test: `src/cursor.rs` tests module

- [ ] **Step 1: Write the failing test** (checksum is timestamp-based, so assert format not value)

```rust
#[test]
fn checksum_ends_with_machine_id_and_uses_url_safe_alphabet() {
    let checksum = build_cursor_checksum("token-abc", "machine-1");
    assert!(checksum.ends_with("machine-1"), "{checksum}");
    let prefix = &checksum[..checksum.len() - "machine-1".len()];
    assert!(prefix.bytes().all(|b| URL_SAFE_BASE64.contains(&b)), "{prefix}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib cursor::tests::checksum_ends_with_machine_id_and_uses_url_safe_alphabet`
Expected: FAIL — not defined.

- [ ] **Step 3: Implement checksum + headers** (port of `jyhEncode`/`buildCursorChecksum`/`__buildCursorHeaders`)

```rust
use std::collections::BTreeMap;
use sha2::{Digest, Sha256};
use crate::types::AvailableAccount;
use crate::config::Config;

pub(crate) const URL_SAFE_BASE64: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn sha256_hex(input: &str) -> String {
    Sha256::digest(input.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn jyh_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let a = bytes[i];
        let b = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
        let c = if i + 2 < bytes.len() { bytes[i + 2] } else { 0 };
        out.push(URL_SAFE_BASE64[(a >> 2) as usize] as char);
        out.push(URL_SAFE_BASE64[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        if i + 1 < bytes.len() { out.push(URL_SAFE_BASE64[(((b & 15) << 2) | (c >> 6)) as usize] as char); }
        if i + 2 < bytes.len() { out.push(URL_SAFE_BASE64[(c & 63) as usize] as char); }
        i += 3;
    }
    out
}

#[must_use]
pub(crate) fn build_cursor_checksum(token: &str, machine_id: &str) -> String {
    let stable = if machine_id.is_empty() { sha256_hex(&format!("{token}machineId")) } else { machine_id.to_string() };
    let timestamp = (chrono::Utc::now().timestamp_millis() / 1_000_000) as u64;
    let mut buf = [
        ((timestamp >> 40) & 0xff) as u8,
        ((timestamp >> 32) & 0xff) as u8,
        ((timestamp >> 24) & 0xff) as u8,
        ((timestamp >> 16) & 0xff) as u8,
        ((timestamp >> 8) & 0xff) as u8,
        (timestamp & 0xff) as u8,
    ];
    let mut prev: u8 = 165;
    for (i, byte) in buf.iter_mut().enumerate() {
        *byte = byte.wrapping_xor(prev).wrapping_add((i % 256) as u8);
        prev = *byte;
    }
    format!("{}{stable}", jyh_encode(&buf))
}

#[must_use]
pub(crate) fn cursor_headers(account: &AvailableAccount, _config: &Config) -> BTreeMap<String, String> {
    let token = &account.token.access_token;
    let meta = account.token.cursor.as_ref();
    let machine_id = meta.and_then(|m| m.service_machine_id.clone())
        .unwrap_or_else(|| if account.account_uuid.is_empty() { account.device_id.clone() } else { account.account_uuid.clone() });
    let client_version = meta.map_or_else(
        || crate::cursor_auth::CURSOR_DEFAULT_CLIENT_VERSION.to_string(),
        |m| if m.client_version.is_empty() { crate::cursor_auth::CURSOR_DEFAULT_CLIENT_VERSION.to_string() } else { m.client_version.clone() });
    let config_version = meta.map_or_else(|| uuid::Uuid::new_v4().to_string(),
        |m| if m.config_version.is_empty() { uuid::Uuid::new_v4().to_string() } else { m.config_version.clone() });
    let session_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, token.as_bytes()).to_string();
    let os = if cfg!(target_os = "macos") { "macos" } else if cfg!(target_os = "windows") { "windows" } else { "linux" };
    BTreeMap::from([
        ("Authorization".into(), format!("Bearer {token}")),
        ("Content-Type".into(), "application/connect+proto".into()),
        ("Accept".into(), "application/connect+proto".into()),
        ("Connect-Protocol-Version".into(), "1".into()),
        ("User-Agent".into(), "connect-es/1.6.1".into()),
        ("x-client-key".into(), sha256_hex(token)),
        ("x-cursor-checksum".into(), build_cursor_checksum(token, &machine_id)),
        ("x-cursor-client-version".into(), client_version),
        ("x-cursor-client-type".into(), "ide".into()),
        ("x-cursor-client-os".into(), os.into()),
        ("x-cursor-config-version".into(), config_version),
        ("x-ghost-mode".into(), "true".into()),
        ("x-session-id".into(), session_id),
        ("x-request-id".into(), uuid::Uuid::new_v4().to_string()),
    ])
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib cursor::tests::checksum_ends_with_machine_id_and_uses_url_safe_alphabet`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cursor.rs
git commit -m "build cursor checksum and request headers"
```

## Task 16: Frame decode + text/reasoning extraction

**Files:**
- Modify: `src/cursor.rs`
- Test: `src/cursor.rs` tests module

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn decodes_text_from_connect_frames() {
    // inner message: field 1 = "hello"
    let mut inner = Vec::new();
    encode_bytes_field(1, b"hello", &mut inner);
    // outer stream message: field 2 = inner
    let mut outer = Vec::new();
    encode_bytes_field(2, &inner, &mut outer);
    let frame = connect_frame(&outer);
    let decoded = decode_cursor_response(&frame);
    assert_eq!(decoded.text, "hello");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib cursor::tests::decodes_text_from_connect_frames`
Expected: FAIL — not defined.

- [ ] **Step 3: Implement decode** (port of `readConnectFrames`/`extractFromPayload`/`decodeCursorResponse`)

```rust
use flate2::read::GzDecoder;
use std::io::Read as _;

#[derive(Debug, Default, Clone)]
pub(crate) struct CursorDecoded { pub text: String, pub reasoning: String, pub error: Option<String> }

struct Frame { kind: u8, payload: Vec<u8> }

fn read_connect_frames(data: &[u8]) -> Vec<Frame> {
    let mut frames = Vec::new();
    let mut pos = 0;
    while pos + 5 <= data.len() {
        let kind = data[pos];
        let len = u32::from_be_bytes([data[pos + 1], data[pos + 2], data[pos + 3], data[pos + 4]]) as usize;
        pos += 5;
        if pos + len > data.len() { break; }
        let mut payload = data[pos..pos + len].to_vec();
        pos += len;
        if kind == 1 || kind == 3 {
            let mut decoded = Vec::new();
            if GzDecoder::new(&payload[..]).read_to_end(&mut decoded).is_ok() { payload = decoded; }
        }
        frames.push(Frame { kind, payload });
    }
    frames
}

fn is_printable(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c == '\t' || c == '\n' || c == '\r' || (' '..='~').contains(&c) || c >= '\u{a0}')
}
fn is_uuid_like(text: &str) -> bool {
    let t = text.trim();
    t.len() >= 32 && t.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}
fn looks_like_proto_start(byte: u8) -> bool {
    let wire = byte & 0x07;
    byte != 0 && matches!(wire, 0 | 1 | 2 | 5)
}

fn extract_inner_text(payload: &[u8], depth: u8) -> String {
    if depth > 4 { return String::new(); }
    let fields = parse_fields(payload);
    if let Some(bytes) = field_bytes(&fields, 1) {
        let text = String::from_utf8_lossy(bytes).to_string();
        if is_printable(&text) && !is_uuid_like(&text) { return text; }
    }
    let mut acc = String::new();
    for f in &fields {
        if let Some(bytes) = &f.bytes
            && bytes.len() > 1 && looks_like_proto_start(bytes[0])
        {
            acc.push_str(&extract_inner_text(bytes, depth + 1));
        }
    }
    acc
}

fn extract_from_payload(payload: &[u8], text: &mut String, reasoning: &mut String) {
    for f in parse_fields(payload) {
        let Some(bytes) = f.bytes else { continue };
        if f.field == 25 {
            reasoning.push_str(&extract_inner_text(&bytes, 0));
        } else if f.field == 1 {
            let direct = String::from_utf8_lossy(&bytes).to_string();
            if is_printable(&direct) && !is_uuid_like(&direct) { text.push_str(&direct); }
        } else if (f.field == 2 || bytes.len() > 1) && looks_like_proto_start(bytes[0]) {
            extract_from_payload(&bytes, text, reasoning);
        }
    }
}

fn extract_json_error(payload: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(payload).ok()?;
    let err = v.get("error")?;
    let code = err.get("code").and_then(Value::as_str);
    let message = err.get("message").and_then(Value::as_str);
    (code.is_some() || message.is_some())
        .then(|| [code, message].into_iter().flatten().collect::<Vec<_>>().join(" — "))
}

#[must_use]
pub(crate) fn decode_cursor_response(data: &[u8]) -> CursorDecoded {
    let mut out = CursorDecoded::default();
    for frame in read_connect_frames(data) {
        if frame.kind == 0 || frame.kind == 1 {
            extract_from_payload(&frame.payload, &mut out.text, &mut out.reasoning);
        } else if (frame.kind == 2 || frame.kind == 3)
            && let Some(err) = extract_json_error(&frame.payload)
        {
            out.error = Some(err);
        }
    }
    // composer/kimi: full answer follows `</think>` inside the reasoning channel
    if out.text.is_empty()
        && let Some(idx) = out.reasoning.to_lowercase().find("</think>")
    {
        let after = idx + "</think>".len();
        out.text = out.reasoning[after..].trim_start().to_string();
        out.reasoning = out.reasoning[..idx].to_string();
    }
    out.text = out.text.trim().to_string();
    out.reasoning = out.reasoning.trim().to_string();
    out
}
```

- [ ] **Step 4: Add a gzip-frame test**

```rust
#[test]
fn decodes_gzipped_frame() {
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write as _;
    let mut inner = Vec::new();
    encode_bytes_field(1, b"zipped", &mut inner);
    let mut payload = Vec::new();
    encode_bytes_field(2, &inner, &mut payload);
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&payload).unwrap();
    let gz = enc.finish().unwrap();
    // frame kind 1 = compressed
    let mut frame = vec![1u8];
    frame.extend_from_slice(&(gz.len() as u32).to_be_bytes());
    frame.extend_from_slice(&gz);
    assert_eq!(decode_cursor_response(&frame).text, "zipped");
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib cursor::tests`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/cursor.rs
git commit -m "decode cursor connect frames"
```

## Task 17: Responses SSE adapter + JSON synthesis

**Files:**
- Modify: `src/cursor.rs`
- Test: `src/cursor.rs` tests module
- Read first: `src/streaming.rs:219-300,530-660` to confirm the event/field names the translators consume.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn synthesizes_responses_json_with_output_text() {
    let decoded = CursorDecoded { text: "hi there".into(), reasoning: String::new(), error: None };
    let payload = synth_responses_json(&decoded, "composer-2.5");
    assert_eq!(payload["object"], "response");
    let text = payload["output"][0]["content"][0]["text"].as_str().unwrap();
    assert_eq!(text, "hi there");
}

#[test]
fn streaming_sse_emits_output_text_delta_and_completed() {
    let chunks = responses_sse_from_decoded("hello", "", "composer-2.5");
    let joined = chunks.join("");
    assert!(joined.contains("response.output_text.delta"), "{joined}");
    assert!(joined.contains("\"delta\":\"hello\""), "{joined}");
    assert!(joined.contains("response.completed"), "{joined}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib cursor::tests::synthesizes_responses_json_with_output_text`
Expected: FAIL — not defined.

- [ ] **Step 3: Implement adapters** (event names match `src/streaming.rs`)

```rust
use serde_json::json;

#[must_use]
pub(crate) fn synth_responses_json(decoded: &CursorDecoded, model: &str) -> Value {
    json!({
        "id": format!("resp_{}", uuid::Uuid::new_v4().simple()),
        "object": "response",
        "model": model,
        "status": "completed",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": decoded.text }]
        }],
        "usage": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0 }
    })
}

fn sse_event(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

/// Build the full Responses-API SSE sequence for one decoded response. Used by the non-incremental
/// test path and as the template for the streaming decoder's per-chunk emission.
#[must_use]
pub(crate) fn responses_sse_from_decoded(text: &str, reasoning: &str, model: &str) -> Vec<String> {
    let mut out = Vec::new();
    out.push(sse_event("response.created", &json!({"type": "response.created",
        "response": {"id": "resp_stream", "object": "response", "model": model, "status": "in_progress", "output": []}})));
    if !reasoning.is_empty() {
        out.push(sse_event("response.reasoning_text.delta",
            &json!({"type": "response.reasoning_text.delta", "delta": reasoning})));
    }
    if !text.is_empty() {
        out.push(sse_event("response.output_text.delta",
            &json!({"type": "response.output_text.delta", "delta": text})));
    }
    out.push(sse_event("response.completed", &json!({"type": "response.completed",
        "response": {"id": "resp_stream", "object": "response", "model": model, "status": "completed",
            "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}],
            "usage": {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}}})));
    out
}
```

> Streaming note for Task 18: the incremental path feeds raw bytes through `read_connect_frames`/`extract_from_payload` as they arrive and emits `response.output_text.delta` / `response.reasoning_text.delta` per new fragment, then one terminal `response.completed`. A simple, correct first version buffers the whole upstream stream, decodes once, and emits `responses_sse_from_decoded(...)` as a single burst — still streamed to the client as SSE. Use the burst version unless real-time deltas are required.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib cursor::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cursor.rs
git commit -m "adapt cursor output to responses sse and json"
```

## Task 18: Replace the stub upstream with the real HTTP/2 calls

**Files:**
- Modify: `src/app.rs` — `HttpUpstreamClient::{cursor_responses, cursor_responses_stream}`; add `send_bytes`/`send_bytes_stream` helpers near `send_json`/`send_stream`.
- Test: `tests/app.rs` (integration via the fake upstream already exercises routing; add a unit check that the real client builds the right request shape is optional — the network call itself is not unit-tested).

- [ ] **Step 1: Write the failing test** — extend the existing `tests/app.rs` fake-upstream cursor test to assert a non-stream `cursor/composer-2.5` chat request returns OpenAI chat shape:

```rust
#[tokio::test]
async fn cursor_chat_returns_chat_completion_shape() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(tmp.path(), &cursor_token()).expect("save");
    // fake upstream cursor_responses returns a Responses JSON object
    let upstream = Arc::new(CapturingUpstream::returning_responses_json());
    let state = cursor_state_with_upstream(tmp.path(), upstream);
    let response = chat_completions(/* State, headers, body */).await; // model: "cursor/composer-2.5"
    let body = response_json(response).await;
    assert_eq!(body["object"], "chat.completion");
}
```

(Adapt to the existing `CapturingUpstream` test harness; the key assertion is that `(Cursor, Chat)` reshapes Responses JSON → `chat.completion` via the shared Codex path.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test app cursor_chat_returns_chat_completion_shape`
Expected: FAIL until the fake returns Responses JSON and arms are exercised (arms already added in Phase 1 — this confirms the wiring).

- [ ] **Step 3: Implement the real client + byte helpers**

Add to `src/app.rs`:

```rust
async fn send_bytes_stream(
    client: reqwest::Client,
    url: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    content_type: &str,
    timeout_ms: u64,
) -> anyhow::Result<UpstreamSseResponse> {
    let mut request = client.post(&url)
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .header(CONTENT_TYPE, content_type)
        .body(body);
    for (key, value) in headers {
        if key.eq_ignore_ascii_case("content-type") { continue; }
        request = request.header(key, value);
    }
    let response = request.send().await?;
    let status = StatusCode::from_u16(response.status().as_u16())?;
    Ok(UpstreamSseResponse { status, body: Box::pin(response.bytes_stream().map_err(anyhow::Error::from)) })
}
```

Replace the stub impls:

```rust
    fn cursor_responses(&self, request: UpstreamRequest) -> UpstreamFuture {
        let client = self.client.clone();
        Box::pin(async move {
            let model = crate::cursor::normalize_model(
                request.body.get("model").and_then(Value::as_str).unwrap_or("cursor/"));
            let frame = crate::cursor::encode_cursor_chat_request(&request.body);
            let headers = crate::cursor::cursor_headers(&request.account, &request.config);
            let timeout_ms = request.config.timeouts.stream_messages_ms;
            let response = send_bytes_stream(client,
                format!("{}{}", crate::cursor::CURSOR_API_BASE_URL, crate::cursor::CURSOR_CHAT_PATH),
                headers, frame, "application/connect+proto", timeout_ms).await?;
            let status = response.status;
            if !status.is_success() {
                let mut body = response.body;
                let mut buf = Vec::new();
                while let Some(chunk) = body.next().await { buf.extend_from_slice(&chunk?); }
                return Ok(UpstreamJsonResponse { status: StatusCode::BAD_GATEWAY,
                    body: json!({"error": {"message": String::from_utf8_lossy(&buf)}}) });
            }
            let mut body = response.body;
            let mut buf = Vec::new();
            while let Some(chunk) = body.next().await { buf.extend_from_slice(&chunk?); }
            let decoded = crate::cursor::decode_cursor_response(&buf);
            if let Some(error) = decoded.error {
                return Ok(UpstreamJsonResponse { status: StatusCode::BAD_GATEWAY,
                    body: json!({"error": {"message": error}}) });
            }
            Ok(UpstreamJsonResponse { status, body: crate::cursor::synth_responses_json(&decoded, &model) })
        })
    }

    fn cursor_responses_stream(&self, request: UpstreamRequest) -> UpstreamSseFuture {
        let client = self.client.clone();
        Box::pin(async move {
            let model = crate::cursor::normalize_model(
                request.body.get("model").and_then(Value::as_str).unwrap_or("cursor/"));
            let frame = crate::cursor::encode_cursor_chat_request(&request.body);
            let headers = crate::cursor::cursor_headers(&request.account, &request.config);
            let timeout_ms = request.config.timeouts.stream_messages_ms;
            let upstream = send_bytes_stream(client,
                format!("{}{}", crate::cursor::CURSOR_API_BASE_URL, crate::cursor::CURSOR_CHAT_PATH),
                headers, frame, "application/connect+proto", timeout_ms).await?;
            let status = upstream.status;
            if !status.is_success() { return Ok(upstream); }
            let model_for_stream = model.clone();
            let sse = Box::pin(try_stream! {
                let mut raw = upstream.body;
                let mut buf = Vec::new();
                while let Some(chunk) = raw.next().await { buf.extend_from_slice(&chunk?); }
                let decoded = crate::cursor::decode_cursor_response(&buf);
                for event in crate::cursor::responses_sse_from_decoded(&decoded.text, &decoded.reasoning, &model_for_stream) {
                    yield Bytes::from(event);
                }
            });
            Ok(UpstreamSseResponse { status, body: sse })
        })
    }
```

Add the missing public consts to `src/cursor.rs`:

```rust
pub const CURSOR_API_BASE_URL: &str = "https://api2.cursor.sh";
pub const CURSOR_CHAT_PATH: &str = "/aiserver.v1.ChatService/StreamUnifiedChatWithTools";
```

(and make `normalize_model`, `encode_cursor_chat_request`, `cursor_headers`, `decode_cursor_response`, `synth_responses_json`, `responses_sse_from_decoded` `pub` so `app.rs` can call them.)

- [ ] **Step 4: Run the test + full suite + clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: PASS, clean.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs src/cursor.rs
git commit -m "implement cursor http2 upstream client"
```

## Task 19: Final verification

- [ ] **Step 1: Full build, lint, test**

Run: `cargo build --release && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: clean release build, no clippy warnings, all tests pass.

- [ ] **Step 2: Manual smoke (requires a real Cursor account)**

```bash
cargo run -- login --provider cursor            # confirm in browser
# or: cargo run -- login --provider cursor --cursor-import-local
cargo run -- serve &
curl -s localhost:8317/v1/models -H "Authorization: Bearer <api-key>" | grep composer-2.5
curl -s localhost:8317/v1/chat/completions -H "Authorization: Bearer <api-key>" \
  -d '{"model":"cursor/composer-2.5","messages":[{"role":"user","content":"say hi"}]}'
```

Expected: `/v1/models` lists `cursor/composer-2.5`; chat returns an assistant message.

- [ ] **Step 3: Commit any final fixes**

```bash
git add -A
git commit -m "finalize cursor provider support"
```

---

## Self-review notes (author)

- **Spec coverage:** all spec sections map to tasks — routing (T4), system fold (T13), ExpiresLead 600s (T5), Option machine id + checksum fallback (T2/T15), stream timeout for both (T18), Composer-focused/no-map (T4/T13), static `/v1/models` (T4/T5), send_bytes helpers (T18), error classification (T8 refresh exhaustion + T18 error frames → 502; per-account backoff kinds inherited from the existing `record_failure` classifier).
- **Type consistency:** `CursorMeta`/`TokenData.cursor`, `ChatMessage`/`Role`, `CursorDecoded`, `CursorPkce`, and fn names (`encode_cursor_chat_request`, `decode_cursor_response`, `synth_responses_json`, `responses_sse_from_decoded`, `cursor_headers`, `refresh_cursor_tokens`, `import_cursor_local`, `poll_cursor_auth`) are used consistently across tasks.
- **Known approximations (flagged, not placeholders):** golden-byte tests are used only for deterministic primitives; encoder/checksum/decoder tests assert structure/format because timestamps + uuids make full output non-deterministic. The streaming path ships as a buffer-then-burst implementation (correct, not truly incremental) — real-time deltas are a documented enhancement, not required for v1.
