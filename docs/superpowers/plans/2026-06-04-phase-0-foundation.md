# Phase 0 — Foundation refactor

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor pengepul's provider model from a closed 3-variant enum into a config-driven `{kind, id}` struct so the codebase can absorb ~30 providers across Phases 1–6 without rewriting types or routing again.

**Architecture:** Replace `ProviderId` enum with `ProviderKind` (small bounded enum, driven by code) + `ProviderId { kind, id: Arc<str> }` (instance, driven by YAML config). Split `upstream.rs`, `translate.rs`, `streaming.rs`, `oauth.rs` into per-kind / per-format modules with thin dispatchers. Make the model-prefix mandatory on every multi-provider route. Migrate `~/.pengepul/<prefix>-<rest>.json` to `~/.pengepul/<id>/<rest>.json` on first start.

**Tech Stack:** Rust 1.96 (edition 2024), axum 0.8, tokio 1, serde / serde_yaml / serde_json, reqwest 0.12, anyhow 1, clap 4.

**Reference spec:** `docs/superpowers/specs/2026-06-04-multi-provider-support-design.md`.

**Non-goals for this phase:** No new provider kinds beyond the existing three (Anthropic, Codex, Opencode). No new auth flows. No format translators for Gemini etc. Those are Phases 1–6.

---

## File structure

After Phase 0 the source tree looks like this:

```
src/
├── lib.rs
├── main.rs
├── app.rs                       # axum router; uses dispatchers from upstream/, translate/, streaming/
├── cli.rs                       # CLI — --provider takes id string
├── config.rs                    # + providers: Vec<ProviderConfig> in Config
├── runtime.rs                   # CLI runtime; login() looks up provider by id from registry
├── service.rs                   # unchanged
├── tokens.rs                    # per-id subdir layout + legacy migration
├── utils.rs                     # unchanged
├── types.rs                     # ProviderKind enum + ProviderId struct
├── providers.rs                 # ProviderRegistry built from Config; route() is prefix-strict
├── accounts.rs                  # AccountManager keyed by &ProviderId; otherwise unchanged
├── upstream/
│   ├── mod.rs                   # build_request dispatcher + shared helpers
│   ├── anthropic.rs             # cloaking, headers, base URL (moved from upstream.rs)
│   ├── codex.rs                 # codex headers/base URL/UA (moved from upstream.rs)
│   └── opencode.rs              # static-key headers, free/paid base URL switch
├── translate/
│   ├── mod.rs                   # to_upstream / from_upstream dispatchers
│   ├── anthropic.rs             # anthropic <-> openai-chat translation (moved)
│   ├── openai_chat.rs           # passthrough helpers (moved)
│   └── openai_responses.rs      # codex Responses translation (moved)
├── streaming/
│   ├── mod.rs                   # SSE dispatcher
│   ├── anthropic.rs             # anthropic SSE <-> openai chunks (moved)
│   ├── openai_chat.rs           # openai chunks helpers (moved)
│   └── openai_responses.rs      # Responses-API SSE (moved)
└── oauth/
    ├── mod.rs                   # shared callback driver, PKCE plumbing, --manual fallback
    ├── flow.rs                  # OAuthConfig, OAuthFlow, RedirectStyle, OAuthProvider trait
    ├── anthropic.rs             # anthropic OAuth impl (moved)
    └── codex.rs                 # codex OAuth impl (moved)
```

Storage layout migrates from flat `~/.pengepul/<prefix>-<rest>.json` to `~/.pengepul/<id>/<rest>.json` on first start of the new binary.

Config gains a `providers:` block; absent block defaults to `[anthropic, codex, opencode]` for back-compat.

---

## Task 1: Add `ProviderKind` enum

**Files:**
- Modify: `src/types.rs:1-46`

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod tests` at the bottom of `src/types.rs`:

```rust
#[test]
fn provider_kind_canonical_ids_are_kebab_case() {
    assert_eq!(ProviderKind::Anthropic.canonical_id(), "anthropic");
    assert_eq!(ProviderKind::Codex.canonical_id(), "codex");
    assert_eq!(ProviderKind::Opencode.canonical_id(), "opencode");
}

#[test]
fn provider_kind_parses_from_str() {
    assert_eq!("anthropic".parse::<ProviderKind>(), Ok(ProviderKind::Anthropic));
    assert_eq!("codex".parse::<ProviderKind>(), Ok(ProviderKind::Codex));
    assert_eq!("opencode".parse::<ProviderKind>(), Ok(ProviderKind::Opencode));
    assert!("nope".parse::<ProviderKind>().is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --locked types::tests
```

Expected: compile error, `ProviderKind` not found.

- [ ] **Step 3: Add the enum**

Insert just above the existing `pub enum ProviderId` definition in `src/types.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    Anthropic,
    Codex,
    Opencode,
}

impl ProviderKind {
    #[must_use]
    pub const fn canonical_id(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.canonical_id())
    }
}

impl FromStr for ProviderKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "anthropic" | "claude" => Ok(Self::Anthropic),
            "codex" => Ok(Self::Codex),
            "opencode" => Ok(Self::Opencode),
            other => Err(format!("unknown provider kind: {other}")),
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test --locked types::tests
```

Expected: both new tests pass; existing tests untouched.

- [ ] **Step 5: Commit**

```bash
git add src/types.rs
git commit -m "add ProviderKind enum"
```

---

## Task 2: Add `ProviderId` struct (and replace existing enum)

**Files:**
- Modify: `src/types.rs` (replace `pub enum ProviderId` and its impls)

- [ ] **Step 1: Write the failing tests**

Add to `src/types.rs::tests`:

```rust
use std::sync::Arc;

#[test]
fn provider_id_struct_round_trips_via_kind() {
    let id = ProviderId::new(ProviderKind::Anthropic, "anthropic");
    assert_eq!(id.kind, ProviderKind::Anthropic);
    assert_eq!(&*id.id, "anthropic");
    assert_eq!(id.to_string(), "anthropic");
}

#[test]
fn provider_id_canonical_helpers_match_kind() {
    assert_eq!(ProviderId::anthropic().kind, ProviderKind::Anthropic);
    assert_eq!(&*ProviderId::anthropic().id, "anthropic");
    assert_eq!(&*ProviderId::codex().id, "codex");
    assert_eq!(&*ProviderId::opencode().id, "opencode");
}

#[test]
fn provider_id_clone_shares_arc() {
    let a = ProviderId::anthropic();
    let b = a.clone();
    assert!(Arc::ptr_eq(&a.id, &b.id));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --locked types::tests::provider_id_struct_round_trips_via_kind
```

Expected: compile error, `ProviderId::new` not found.

- [ ] **Step 3: Replace the enum with a struct**

In `src/types.rs`, delete the existing `pub enum ProviderId { Anthropic, Codex, Opencode }` and its `impl ProviderId { storage_prefix }`, `impl fmt::Display for ProviderId`, `impl FromStr for ProviderId`. Replace with:

```rust
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderId {
    pub kind: ProviderKind,
    pub id: Arc<str>,
}

impl ProviderId {
    #[must_use]
    pub fn new(kind: ProviderKind, id: impl Into<Arc<str>>) -> Self {
        Self { kind, id: id.into() }
    }

    #[must_use]
    pub fn anthropic() -> Self { Self::new(ProviderKind::Anthropic, "anthropic") }
    #[must_use]
    pub fn codex() -> Self     { Self::new(ProviderKind::Codex, "codex") }
    #[must_use]
    pub fn opencode() -> Self  { Self::new(ProviderKind::Opencode, "opencode") }

    /// Subdirectory under `auth_dir` where this provider's credential files live.
    #[must_use]
    pub fn storage_dir(&self) -> &str { &self.id }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.id)
    }
}

impl FromStr for ProviderId {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let kind = value.parse::<ProviderKind>()?;
        Ok(Self::new(kind, value))
    }
}
```

Also drop the existing `provider_id_parses_and_displays` test (the assertions about `storage_prefix()` no longer apply — `storage_dir()` returns the id, not a hard-coded prefix).

- [ ] **Step 4: Update field-by-field token usage sites**

This task introduces `ProviderId` as a non-`Copy` type. The compiler will flag every usage. Fix each by replacing `ProviderId::Anthropic` → `ProviderId::anthropic()`, `ProviderId::Codex` → `ProviderId::codex()`, `ProviderId::Opencode` → `ProviderId::opencode()`, and `== ProviderId::X` → `.kind == ProviderKind::X`.

Sites to update (search with `rg 'ProviderId::(Anthropic|Codex|Opencode)' src/`):

- `src/tokens.rs:110-127` (token_to_storage / storage_to_token match arms — kept for back-compat reading; see Task 4)
- `src/accounts.rs:444-446` (chatgpt_account_id check)
- `src/runtime.rs:54, 130-162, 237-255, 276` (login flow, helpers)
- `src/upstream.rs` (Opencode-specific code paths; will move in Task 14)
- `src/oauth.rs` (provider field on TokenData; will refactor in Task 20)
- `src/providers.rs` (build_registry; will rewrite in Task 9)
- `src/cli.rs` (--provider parsing; will refactor in Task 21)
- `src/app.rs` (account manager keys; will refactor in Task 13)

Each fix is mechanical: `cargo check --locked` after a batch of edits, repeat until clean.

- [ ] **Step 5: Run all tests**

```bash
cargo test --locked
```

Expected: PASS. Behavior is unchanged; only the type shape has changed.

- [ ] **Step 6: Run clippy**

```bash
cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS. If clippy complains about `ProviderId` clones being needless in places where `&ProviderId` would do, accept the warning notes for now; later tasks will narrow the references.

- [ ] **Step 7: Commit**

```bash
git add src/
git commit -m "replace ProviderId enum with {kind, id} struct"
```

---

## Task 3: `storage_dir()` drives tokens.rs filenames

**Files:**
- Modify: `src/tokens.rs:24-46` (save_token)
- Modify: `src/tokens.rs:55-102` (load_all_tokens)
- Modify: `src/tokens.rs:104-143` (token_to_storage / storage_to_token)

This task only updates `tokens.rs` to use `provider.storage_dir()` consistently and removes the now-unused legacy `storage_prefix()` helper. The actual per-id subdirectory layout comes in Tasks 5–7.

- [ ] **Step 1: Write a failing test**

Add to `src/tokens.rs` (create a `#[cfg(test)] mod tests` block at the end if none exists):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ProviderId, TokenData, ProviderKind};

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
        let path = save_token(dir.path(), &token(ProviderId::anthropic(), "alice@x.com"))
            .expect("save");
        let filename = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(filename.starts_with("anthropic-"), "filename was {filename}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --locked tokens::tests::save_token_uses_storage_dir_as_filename_prefix
```

Expected: FAIL — current filename uses `claude-` prefix.

- [ ] **Step 3: Update `save_token` to use `storage_dir`**

In `src/tokens.rs:35-39`, change:

```rust
let filename = format!(
    "{}-{}.json",
    token.provider.storage_prefix(),
    sanitize_email(&token.email)
);
```

to:

```rust
let filename = format!(
    "{}-{}.json",
    token.provider.storage_dir(),
    sanitize_email(&token.email)
);
```

- [ ] **Step 4: Update `load_all_tokens` prefix matching**

In `src/tokens.rs:60`, change:

```rust
let prefix = provider.map(ProviderId::storage_prefix);
```

to (note the `as_ref` so we don't move):

```rust
let prefix = provider.as_ref().map(|p| p.storage_dir().to_string());
```

In the loop at lines 79-88, change:

```rust
if let Some(prefix) = prefix {
    if !filename.starts_with(&format!("{prefix}-")) {
        continue;
    }
} else if !(filename.starts_with("claude-")
    || filename.starts_with("codex-")
    || filename.starts_with("opencode-"))
{
    continue;
}
```

to:

```rust
if let Some(prefix) = &prefix {
    if !filename.starts_with(&format!("{prefix}-")) {
        continue;
    }
} else if !(filename.starts_with("anthropic-")
    || filename.starts_with("claude-")        // legacy
    || filename.starts_with("codex-")
    || filename.starts_with("opencode-"))
{
    continue;
}
```

Also change the signature `provider: Option<ProviderId>` to `provider: Option<&ProviderId>` and update the call site in `src/accounts.rs::load` and `::reload` to pass `Some(&self.provider)`.

The final `if provider.is_none_or(...)` comparison becomes `if provider.is_none_or(|provider| token.provider.kind == provider.kind)` — we filter on kind, not the whole id, because legacy `claude-*.json` files store `type: "claude"` and parse to `kind: Anthropic`.

- [ ] **Step 5: Update `token_to_storage` / `storage_to_token`**

Replace the `match token.provider { ProviderId::Anthropic => ... }` arms with a kind-based match:

```rust
fn token_to_storage(token: &TokenData) -> StoredToken {
    StoredToken {
        access_token: token.access_token.clone(),
        refresh_token: token.refresh_token.clone(),
        email: Some(token.email.clone()),
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
    // ... rest unchanged
}
```

Remove the now-unused `ProviderId::storage_prefix` helper from `src/types.rs` (it was replaced by `storage_dir`).

- [ ] **Step 6: Run all tests**

```bash
cargo test --locked
```

Expected: PASS. New test passes; existing token round-trip tests still pass.

- [ ] **Step 7: Commit**

```bash
git add src/types.rs src/tokens.rs src/accounts.rs
git commit -m "use ProviderId::storage_dir for token filenames"
```

---

## Task 4: Per-id subdirectory write

**Files:**
- Modify: `src/tokens.rs::save_token`

- [ ] **Step 1: Write the failing test**

Add to `src/tokens.rs::tests`:

```rust
#[test]
fn save_token_writes_under_per_id_subdirectory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = save_token(dir.path(), &token(ProviderId::anthropic(), "alice@x.com"))
        .expect("save");
    let relative = path.strip_prefix(dir.path()).expect("under dir");
    let components: Vec<_> = relative.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    assert_eq!(components.len(), 2, "{components:?}");
    assert_eq!(components[0], "anthropic");
    assert!(components[1].ends_with(".json"));
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --locked tokens::tests::save_token_writes_under_per_id_subdirectory
```

Expected: FAIL — current implementation writes flat under `auth_dir`.

- [ ] **Step 3: Update `save_token`**

Replace the body of `save_token`:

```rust
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
```

The earlier `save_token_uses_storage_dir_as_filename_prefix` test (from Task 3) no longer matches — filenames are no longer prefixed. Delete that test in this commit.

- [ ] **Step 4: Run tests**

```bash
cargo test --locked
```

Expected: PASS. The new test asserts the directory layout.

- [ ] **Step 5: Commit**

```bash
git add src/tokens.rs
git commit -m "write tokens under per-id subdirectory"
```

---

## Task 5: Per-id subdirectory read

**Files:**
- Modify: `src/tokens.rs::load_all_tokens`

- [ ] **Step 1: Write the failing test**

Add to `src/tokens.rs::tests`:

```rust
#[test]
fn load_all_tokens_reads_per_id_subdirectories() {
    let dir = tempfile::tempdir().expect("tempdir");
    save_token(dir.path(), &token(ProviderId::anthropic(), "alice@x.com")).expect("save anthropic");
    save_token(dir.path(), &token(ProviderId::codex(), "bob@y.com")).expect("save codex");

    let all = load_all_tokens(dir.path(), None).expect("load");
    assert_eq!(all.len(), 2);
    let kinds: Vec<_> = all.iter().map(|t| t.provider.kind).collect();
    assert!(kinds.contains(&ProviderKind::Anthropic));
    assert!(kinds.contains(&ProviderKind::Codex));

    let just_anthropic = load_all_tokens(dir.path(), Some(&ProviderId::anthropic())).expect("load anthropic");
    assert_eq!(just_anthropic.len(), 1);
    assert_eq!(just_anthropic[0].provider.kind, ProviderKind::Anthropic);
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --locked tokens::tests::load_all_tokens_reads_per_id_subdirectories
```

Expected: FAIL — current implementation reads flat from `auth_dir`.

- [ ] **Step 3: Rewrite `load_all_tokens`**

Replace the body:

```rust
pub fn load_all_tokens(auth_dir: &Path, provider: Option<&ProviderId>) -> Result<Vec<TokenData>> {
    if !auth_dir.exists() {
        return Ok(Vec::new());
    }

    let scan_dirs: Vec<PathBuf> = if let Some(provider) = provider {
        vec![auth_dir.join(provider.storage_dir())]
    } else {
        // Scan every direct subdirectory; non-dir entries (including legacy flat files) are ignored.
        fs::read_dir(auth_dir)
            .with_context(|| format!("failed to read {}", auth_dir.display()))?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|entry| entry.path())
            .collect()
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
            let Some(stored) = fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<StoredToken>(&text).ok())
            else {
                continue;
            };
            let token = storage_to_token(stored);
            if provider.is_none_or(|p| token.provider.kind == p.kind) {
                tokens.push(token);
            }
        }
    }
    Ok(tokens)
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/tokens.rs
git commit -m "read tokens from per-id subdirectories"
```

---

## Task 6: Legacy storage migration

**Files:**
- Modify: `src/tokens.rs` (add `migrate_legacy_layout` function)
- Modify: `src/app.rs` (call migration once at startup before any account loading)

- [ ] **Step 1: Write the failing test**

Add to `src/tokens.rs::tests`:

```rust
#[test]
fn migrate_legacy_layout_moves_files_into_subdirs_idempotently() {
    let dir = tempfile::tempdir().expect("tempdir");
    let auth = dir.path();
    std::fs::create_dir_all(auth).unwrap();
    std::fs::write(auth.join("claude-alice_at_x.com.json"), r#"{"access_token":"a","refresh_token":"r","expired":"2099-01-01T00:00:00Z"}"#).unwrap();
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
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --locked tokens::tests::migrate_legacy_layout_moves_files_into_subdirs_idempotently
```

Expected: FAIL — `migrate_legacy_layout` doesn't exist.

- [ ] **Step 3: Implement migration**

Add to `src/tokens.rs`:

```rust
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
    let mappings = [("claude-", "anthropic"), ("codex-", "codex"), ("opencode-", "opencode")];
    let mut moved = 0;
    for entry in fs::read_dir(auth_dir).with_context(|| format!("failed to read {}", auth_dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() { continue; }
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else { continue; };
        if !filename.ends_with(".json") { continue; }
        for (prefix, dest_dir) in mappings {
            if let Some(rest) = filename.strip_prefix(prefix) {
                let dest = auth_dir.join(dest_dir);
                fs::create_dir_all(&dest).with_context(|| format!("failed to create {}", dest.display()))?;
                set_mode(&dest, 0o700)?;
                let new_path = dest.join(rest);
                fs::rename(&path, &new_path)
                    .with_context(|| format!("failed to move {} -> {}", path.display(), new_path.display()))?;
                moved += 1;
                tracing::info!(from = %path.display(), to = %new_path.display(), "migrated legacy token file");
                break;
            }
        }
    }
    Ok(moved)
}
```

- [ ] **Step 4: Run the test**

```bash
cargo test --locked tokens::tests::migrate_legacy_layout_moves_files_into_subdirs_idempotently
```

Expected: PASS.

- [ ] **Step 5: Call migration at startup**

In `src/app.rs`, find the function that builds `AppState` (search for `pub fn create_app` or where accounts are first loaded). Before any `AccountManager::load()` call, add:

```rust
if let Err(error) = crate::tokens::migrate_legacy_layout(&config.auth_dir) {
    tracing::warn!(?error, "legacy token layout migration failed");
}
```

The migration is best-effort: if it fails, the user gets a warning but startup continues.

- [ ] **Step 6: Run all tests + clippy**

```bash
cargo test --locked && cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/tokens.rs src/app.rs
git commit -m "migrate legacy auth-dir layout into per-id subdirs"
```

---

## Task 7: `providers:` block in config (with default)

**Files:**
- Modify: `src/config.rs` (add raw + public types + default)

- [ ] **Step 1: Write the failing tests**

Create `src/config.rs::tests` (append if existing tests are absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn load(text: &str, dir: &Path) -> Config {
        let path = dir.join("config.yaml");
        std::fs::write(&path, text).unwrap();
        load_config(Some(&path), Some(dir), dir).expect("load")
    }

    #[test]
    fn config_with_no_providers_block_defaults_to_anthropic_codex_opencode() {
        let dir = tempdir().unwrap();
        let config = load("port: 8317\napi-keys: [sk-x]\n", dir.path());
        let ids: Vec<&str> = config.providers.iter().map(|p| &*p.id).collect();
        assert_eq!(ids, vec!["anthropic", "codex", "opencode"]);
        for provider in &config.providers {
            assert!(matches!(
                provider.kind,
                crate::types::ProviderKind::Anthropic
                    | crate::types::ProviderKind::Codex
                    | crate::types::ProviderKind::Opencode
            ));
        }
    }

    #[test]
    fn explicit_providers_block_wins() {
        let dir = tempdir().unwrap();
        let yaml = r#"
port: 8317
api-keys: [sk-x]
providers:
  - id: anthropic
    kind: anthropic
"#;
        let config = load(yaml, dir.path());
        assert_eq!(config.providers.len(), 1);
        assert_eq!(&*config.providers[0].id, "anthropic");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --locked config::tests
```

Expected: FAIL — `config.providers` does not exist.

- [ ] **Step 3: Add types and parsing**

In `src/config.rs`, after `CloakingConfig`, add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    pub id: Arc<str>,
    pub kind: crate::types::ProviderKind,
}
```

Add `use std::sync::Arc;` and `use crate::types::ProviderKind;` at the top.

Extend `pub struct Config`:

```rust
pub providers: Vec<ProviderConfig>,
```

Extend `RawConfig`:

```rust
#[serde(default)]
providers: Option<Vec<RawProviderEntry>>,
```

And the new raw entry:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProviderEntry {
    id: String,
    kind: crate::types::ProviderKind,
}
```

(`#[serde(default)]` on the field means missing `providers:` becomes `None`.)

In `RawConfig::default`, add `providers: None,`.

In `load_config`, after building everything else but before the final `Ok(Config { ... })`, compute:

```rust
let providers = raw.providers.unwrap_or_else(default_providers);
let providers = providers
    .into_iter()
    .map(|raw| ProviderConfig { id: Arc::from(raw.id), kind: raw.kind })
    .collect();
```

And include `providers,` in the returned `Config`.

Add a helper:

```rust
fn default_providers() -> Vec<RawProviderEntry> {
    vec![
        RawProviderEntry { id: "anthropic".into(), kind: ProviderKind::Anthropic },
        RawProviderEntry { id: "codex".into(),     kind: ProviderKind::Codex },
        RawProviderEntry { id: "opencode".into(),  kind: ProviderKind::Opencode },
    ]
}
```

- [ ] **Step 4: Run tests + clippy**

```bash
cargo test --locked && cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "add providers config block with anthropic/codex/opencode default"
```

---

## Task 8: `ProviderRegistry` built from config

**Files:**
- Modify: `src/providers.rs` (rewrite `build_registry` and lookup methods)
- Modify: `src/runtime.rs:54` (pass `&Config` to where `ProviderRegistry` is built)

- [ ] **Step 1: Write the failing tests**

Replace `src/providers.rs::tests` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderConfig;
    use crate::types::ProviderKind;
    use std::sync::Arc;

    fn registry(entries: &[(&str, ProviderKind)]) -> ProviderRegistry {
        let providers = entries
            .iter()
            .map(|(id, kind)| ProviderConfig { id: Arc::from(*id), kind: *kind })
            .collect::<Vec<_>>();
        ProviderRegistry::from_config(&providers)
    }

    #[test]
    fn finds_provider_by_id() {
        let r = registry(&[
            ("anthropic", ProviderKind::Anthropic),
            ("codex", ProviderKind::Codex),
            ("opencode", ProviderKind::Opencode),
        ]);
        assert_eq!(r.by_id("anthropic").map(|p| p.kind), Some(ProviderKind::Anthropic));
        assert_eq!(r.by_id("opencode").map(|p| p.kind), Some(ProviderKind::Opencode));
        assert!(r.by_id("missing").is_none());
    }

    #[test]
    fn lists_all_providers() {
        let r = registry(&[("anthropic", ProviderKind::Anthropic)]);
        let all: Vec<&str> = r.all().iter().map(|p| &*p.id).collect();
        assert_eq!(all, vec!["anthropic"]);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --locked providers::tests
```

Expected: FAIL — `from_config`, `by_id` not defined.

- [ ] **Step 3: Rewrite `providers.rs`**

Replace the file body (keep `OPENCODE_PREFIX`, `OPENCODE_MODELS`, `OPENCODE_FREE_MODELS`, `strip_opencode_prefix`, `is_opencode_free_model` for now; they'll move in Task 14):

```rust
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::config::ProviderConfig;
use crate::types::{ProviderId, ProviderKind};

#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    providers: Vec<ProviderId>,
    by_id: BTreeMap<Arc<str>, ProviderId>,
}

impl ProviderRegistry {
    #[must_use]
    pub fn from_config(entries: &[ProviderConfig]) -> Self {
        let providers: Vec<ProviderId> = entries
            .iter()
            .map(|entry| ProviderId::new(entry.kind, entry.id.clone()))
            .collect();
        let by_id = providers
            .iter()
            .map(|provider| (provider.id.clone(), provider.clone()))
            .collect();
        Self { providers, by_id }
    }

    #[must_use]
    pub fn all(&self) -> &[ProviderId] {
        &self.providers
    }

    /// Look up a provider by its configured id.
    #[must_use]
    pub fn by_id(&self, id: &str) -> Option<&ProviderId> {
        self.by_id.get(id)
    }
}
```

Delete the existing `for_model`, `get`, `build_registry`, the static `Provider` struct, the `anthropic_matches_model` / `codex_matches_model` / `opencode_matches_model` regex helpers, and the old tests. Routing comes back in Task 10 in strict-prefix form.

- [ ] **Step 4: Update `lib.rs` if needed**

`lib.rs` already declares `pub mod providers;` — no change needed.

- [ ] **Step 5: Update `runtime.rs` and `cli.rs`**

`src/runtime.rs:54` currently takes `&ProviderRegistry` and ignores it. `src/cli.rs` builds the registry via `build_registry(auth_dir)`. Replace those calls with `ProviderRegistry::from_config(&config.providers)`. Search for `build_registry` and update both sites.

- [ ] **Step 6: Run all tests + clippy**

```bash
cargo test --locked && cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS. Routing tests will fail until Task 10 lands; if any existing test depends on `for_model`, delete it now — the new strict-prefix routing has its own tests.

- [ ] **Step 7: Commit**

```bash
git add src/providers.rs src/runtime.rs src/cli.rs
git commit -m "rebuild ProviderRegistry from config"
```

---

## Task 9: Strict prefix routing

**Files:**
- Modify: `src/providers.rs::ProviderRegistry` (add `route` + `strip_provider_prefix` helper)

- [ ] **Step 1: Write the failing tests**

Add to `src/providers.rs::tests`:

```rust
#[test]
fn route_requires_explicit_prefix() {
    let r = registry(&[
        ("anthropic", ProviderKind::Anthropic),
        ("codex", ProviderKind::Codex),
        ("opencode", ProviderKind::Opencode),
    ]);
    assert_eq!(r.route("anthropic/claude-sonnet-4-6").map(|p| p.kind), Some(ProviderKind::Anthropic));
    assert_eq!(r.route("codex/gpt-5.5").map(|p| p.kind), Some(ProviderKind::Codex));
    assert_eq!(r.route("opencode/glm-5.1").map(|p| p.kind), Some(ProviderKind::Opencode));

    // No implicit fallback for bare model ids.
    assert!(r.route("claude-sonnet-4-6").is_none());
    assert!(r.route("gpt-5.5").is_none());
}

#[test]
fn strip_provider_prefix_removes_first_slash_segment() {
    assert_eq!(strip_provider_prefix("anthropic/claude-sonnet-4-6"), "claude-sonnet-4-6");
    assert_eq!(strip_provider_prefix("opencode/glm-5.1"), "glm-5.1");
    assert_eq!(strip_provider_prefix("no-prefix-here"), "no-prefix-here");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --locked providers::tests
```

Expected: FAIL — `route`, `strip_provider_prefix` not defined.

- [ ] **Step 3: Add `route` and `strip_provider_prefix`**

In `src/providers.rs`, on `impl ProviderRegistry`:

```rust
/// Route `model` to a provider by its explicit prefix (`<id>/<model>`).
///
/// Returns `None` for bare model ids; clients must supply a prefix.
#[must_use]
pub fn route(&self, model: &str) -> Option<&ProviderId> {
    let (prefix, _) = model.split_once('/')?;
    self.by_id(prefix)
}
```

Below the registry:

```rust
/// Strip the `<id>/` routing prefix to get the upstream model id. Returns the input unchanged
/// when no prefix is present.
#[must_use]
pub fn strip_provider_prefix(model: &str) -> &str {
    model.split_once('/').map_or(model, |(_, rest)| rest)
}
```

Replace existing call sites of `strip_opencode_prefix` with `strip_provider_prefix` (it's used in `src/upstream.rs` and `src/providers.rs`). The opencode-specific constant `OPENCODE_PREFIX` is now unused — delete it and `strip_opencode_prefix`.

- [ ] **Step 4: Run tests**

```bash
cargo test --locked providers::tests
```

Expected: PASS.

- [ ] **Step 5: Update routing call sites in `app.rs`**

`src/app.rs` routes requests to provider-specific upstream paths. Find every place that looks at the request body's `model` field to decide which upstream to use (search for `for_model` if it still exists, or the place that picks between anthropic / codex / opencode). Replace with `registry.route(model)` and `unwrap_or` to return a 400 to the client when the prefix is missing.

Add the 400 path: when `route` returns `None`, return an `axum::http::StatusCode::BAD_REQUEST` with JSON body:

```json
{ "error": { "type": "invalid_request_error", "message": "model id must include a provider prefix (e.g. anthropic/claude-sonnet-4-6)" } }
```

- [ ] **Step 6: Run full quality gates**

```bash
cargo fmt --check && cargo test --locked && cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/providers.rs src/app.rs src/upstream.rs
git commit -m "require model prefix on every route"
```

---

## Task 10: AccountManager registry keyed by `ProviderId`

**Files:**
- Modify: `src/app.rs` (replace named per-provider managers with `HashMap<ProviderId, AccountManager>`)
- Modify: `src/accounts.rs:98-106` (no change to fields — `provider: ProviderId` still holds a struct)

- [ ] **Step 1: Identify current state**

`src/app.rs` (the 2256-LOC behemoth) currently holds account managers in `AppState`. Find the struct and the three named fields (probably `anthropic_accounts`, `codex_accounts`, `opencode_accounts` or similar). The exact field names will be visible after running:

```bash
rg -n 'AccountManager' src/app.rs | head
```

- [ ] **Step 2: Add a public accessor on `AppState` and write the failing test**

This task's verification needs visibility into AppState. Add to `src/app.rs`:

```rust
impl AppState {
    /// Ids of every provider this AppState has an AccountManager for.
    #[must_use]
    pub fn provider_ids(&self) -> Vec<std::sync::Arc<str>> {
        let mut ids: Vec<_> = self.account_managers.keys().map(|p| p.id.clone()).collect();
        ids.sort();
        ids
    }
}
```

Then add to `src/app.rs::tests`:

```rust
#[test]
fn account_managers_are_built_for_each_configured_provider() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = crate::config::load_config(None, Some(dir.path()), dir.path()).expect("config");
    config.providers = vec![
        crate::config::ProviderConfig { id: "anthropic".into(), kind: crate::types::ProviderKind::Anthropic },
        crate::config::ProviderConfig { id: "codex".into(),     kind: crate::types::ProviderKind::Codex },
        crate::config::ProviderConfig { id: "opencode".into(),  kind: crate::types::ProviderKind::Opencode },
    ];
    let state = AppState::new(config).expect("appstate");
    let ids: Vec<String> = state.provider_ids().into_iter().map(|id| id.to_string()).collect();
    assert_eq!(ids, vec!["anthropic", "codex", "opencode"]);
}
```

If `AppState::new(config)` does not exist yet, also expose it as the public constructor that wraps whatever `create_app` does internally. This is a deliberately small public surface added for testability.

- [ ] **Step 3: Update `AppState`**

Replace the three named manager fields with:

```rust
account_managers: std::collections::HashMap<ProviderId, AccountManager>,
```

Build it by iterating `config.providers` and constructing one manager per entry. The refresh callback for each is selected by `kind`:

```rust
let mut account_managers = HashMap::new();
for entry in &config.providers {
    let provider = ProviderId::new(entry.kind, entry.id.clone());
    let refresh: RefreshFn = match entry.kind {
        ProviderKind::Anthropic => /* existing anthropic refresh closure */,
        ProviderKind::Codex     => /* existing codex refresh closure */,
        ProviderKind::Opencode  => Box::new(|_| Box::pin(async { bail!("opencode keys do not refresh") })),
    };
    let policy = match entry.kind {
        ProviderKind::Opencode => RefreshPolicy { kind: RefreshPolicyKind::Never, seconds: 0 },
        _ => RefreshPolicy::default(),
    };
    let mut manager = AccountManager::new(config.auth_dir.clone(), provider.clone(), refresh, policy);
    manager.load().context("loading provider accounts")?;
    account_managers.insert(provider, manager);
}
```

- [ ] **Step 4: Update lookup sites**

Wherever the code did `state.anthropic_accounts.next_account()`, change to `state.manager(&provider).next_account()` with:

```rust
impl AppState {
    pub fn manager(&self, provider: &ProviderId) -> &AccountManager { &self.account_managers[provider] }
    pub fn manager_mut(&mut self, provider: &ProviderId) -> &mut AccountManager {
        self.account_managers.get_mut(provider).expect("manager for configured provider")
    }
}
```

Concurrency: today's mutexes on the per-provider managers move with them. Whatever lock wrapper existed (`Mutex<AccountManager>` or similar) stays unchanged inside the HashMap value.

- [ ] **Step 5: Run all tests**

```bash
cargo test --locked
```

Expected: PASS. Most existing tests don't touch AppState's manager fields by name; if any do, update them to use the new accessor.

- [ ] **Step 6: Run clippy**

```bash
cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/app.rs src/accounts.rs tests/
git commit -m "key AccountManager registry by ProviderId"
```

---

## Task 11: `/v1/models` returns namespaced ids

**Files:**
- Modify: `src/app.rs` (the `/v1/models` handler)

- [ ] **Step 1: Find the handler**

```bash
rg -n '/v1/models' src/app.rs
```

The handler is the function bound to the `/v1/models` route.

- [ ] **Step 2: Extract a pure model-list helper and write the failing test**

Extract the model-list construction into a pure function that takes a `&ProviderRegistry` and returns the response JSON. Test the pure function directly.

Add to `src/app.rs` (above the handler):

```rust
fn build_models_response(registry: &crate::providers::ProviderRegistry) -> serde_json::Value {
    let mut data = Vec::new();
    for provider in registry.all() {
        for model in static_models_for(&provider.kind) {
            data.push(serde_json::json!({
                "id": format!("{}/{}", provider.id, model),
                "object": "model",
                "owned_by": &*provider.id,
            }));
        }
    }
    serde_json::json!({ "object": "list", "data": data })
}

const fn static_models_for(kind: &crate::types::ProviderKind) -> &'static [&'static str] {
    match kind {
        crate::types::ProviderKind::Anthropic => &[
            "claude-opus-4-7", "claude-sonnet-4-6", "claude-haiku-4-5",
        ],
        crate::types::ProviderKind::Codex => &["gpt-5.5", "gpt-5.4", "gpt-5"],
        crate::types::ProviderKind::Opencode => &[
            // Combine OPENCODE_MODELS + OPENCODE_FREE_MODELS as a static array.
            // (After Task 14 these live in upstream::opencode; reference them from there.)
            "glm-5.1", "glm-5", "kimi-k2.6", "kimi-k2.5",
            "deepseek-v4-pro", "deepseek-v4-flash",
            "minimax-m2.7", "minimax-m2.5",
            "qwen3.7-max", "qwen3.6-plus", "qwen3.5-plus",
            "mimo-v2.5-pro", "mimo-v2.5", "mimo-v2-pro", "mimo-v2-omni",
            "deepseek-v4-flash-free", "mimo-v2.5-free",
            "qwen3.6-plus-free", "minimax-m3-free", "nemotron-3-super-free",
        ],
    }
}
```

Add to `src/app.rs::tests`:

```rust
#[test]
fn v1_models_response_namespaces_every_id() {
    let providers = vec![
        crate::config::ProviderConfig { id: "anthropic".into(), kind: crate::types::ProviderKind::Anthropic },
        crate::config::ProviderConfig { id: "codex".into(),     kind: crate::types::ProviderKind::Codex },
        crate::config::ProviderConfig { id: "opencode".into(),  kind: crate::types::ProviderKind::Opencode },
    ];
    let registry = crate::providers::ProviderRegistry::from_config(&providers);
    let response = build_models_response(&registry);
    let ids: Vec<String> = response["data"].as_array().unwrap()
        .iter().map(|m| m["id"].as_str().unwrap().to_string()).collect();
    assert!(!ids.is_empty());
    for id in &ids {
        assert!(id.contains('/'), "every id must be prefixed: {id}");
    }
    assert!(ids.iter().any(|id| id.starts_with("anthropic/")));
    assert!(ids.iter().any(|id| id.starts_with("codex/")));
    assert!(ids.iter().any(|id| id.starts_with("opencode/")));
}
```

- [ ] **Step 3: Run test to verify it fails**

```bash
cargo test --locked app::tests::v1_models_returns_prefixed_ids
```

Expected: FAIL.

- [ ] **Step 4: Update the handler**

Replace the body of the `/v1/models` handler with:

```rust
async fn list_models(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(build_models_response(&state.registry))
}
```

`state.registry` must be exposed on `AppState` (a `ProviderRegistry` field built from `config.providers`; add the field if it isn't already there).

Generic per-provider lazy-fetch of upstream `/v1/models` comes in Phase 1.

- [ ] **Step 5: Run tests + clippy**

```bash
cargo test --locked && cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/app.rs
git commit -m "namespace /v1/models ids with provider prefix"
```

---

## Task 12: Split `upstream.rs` into per-kind modules

**Files:**
- Create: `src/upstream/mod.rs`
- Create: `src/upstream/anthropic.rs`
- Create: `src/upstream/codex.rs`
- Create: `src/upstream/opencode.rs`
- Delete: `src/upstream.rs`
- Modify: `src/lib.rs` (declare the module)

- [ ] **Step 1: Move existing code verbatim**

Create `src/upstream/anthropic.rs` and copy the Anthropic-specific functions from `src/upstream.rs`: `ANTHROPIC_BASE_URL`, `ANTHROPIC_OAUTH_BETA`, `build_beta_header`, `anthropic_headers`, `apply_cloaking`, `session_id`, `passthrough_anthropic_headers`, `billing_header`, `first_user_text`, `extract_api_key`, `header_value`, `FINGERPRINT_SALT`, `SESSIONS`, `Sessions`, `timeout_seconds`, `stainless_os`, `stainless_arch`.

Create `src/upstream/codex.rs` and copy: `CODEX_BASE_URL`, `CODEX_RESPONSES_PATH`, `CODEX_MODELS_PATH`, `CODEX_DEFAULT_ORIGINATOR`, `CODEX_DEFAULT_CLI_VERSION`, `normalize_codex_responses_body`, `codex_headers`, `codex_user_agent`, `codex_os`, `codex_arch`.

Create `src/upstream/opencode.rs` and copy: `OPENCODE_BASE_URL`, `OPENCODE_ZEN_BASE_URL`, `opencode_base_url`, `opencode_headers`. Also move `OPENCODE_MODELS`, `OPENCODE_FREE_MODELS`, `is_opencode_free_model` here from `providers.rs` (they're opencode-specific).

Create `src/upstream/mod.rs` with `pub mod anthropic;`, `pub mod codex;`, `pub mod opencode;`, plus shared helpers `header_value`, `extract_api_key`, `timeout_seconds`, `stainless_os`, `stainless_arch` (or keep them inside `anthropic.rs` and re-export — pick one).

Delete `src/upstream.rs`.

In `src/lib.rs`, replace `pub mod upstream;` (still works, points to `upstream/mod.rs`).

- [ ] **Step 2: Update import sites**

Wherever the codebase imported `crate::upstream::{anthropic_headers, ...}`, change to `crate::upstream::anthropic::headers` (or whatever the new path is — settle on `module::headers` rather than `module::anthropic_headers`).

- [ ] **Step 3: Run quality gates**

```bash
cargo fmt --check && cargo test --locked && cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS — this is a no-behavior-change refactor.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "split upstream.rs into per-kind modules"
```

---

## Task 13: `upstream::build_request` dispatcher

**Files:**
- Modify: `src/upstream/mod.rs`
- Modify: `src/app.rs` (call dispatcher instead of per-kind helpers directly)

- [ ] **Step 1: Write the failing test**

Add to `src/upstream/mod.rs::tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProviderId;

    #[test]
    fn dispatcher_picks_kind_specific_base_url() {
        // Build a minimal RequestCtx and AvailableAccount stubs.
        // Assert that build_request(provider, ...) returns a URL containing the kind's host.
        // ...
    }
}
```

This test is light because most of the per-kind logic is unchanged from Task 12. The dispatcher itself is the only new wiring.

- [ ] **Step 2: Add the dispatcher**

In `src/upstream/mod.rs`:

```rust
pub struct UpstreamRequest {
    pub method: reqwest::Method,
    pub url: String,
    pub headers: std::collections::BTreeMap<String, String>,
    pub body: serde_json::Value,
}

pub struct RequestCtx<'a> {
    pub config: &'a crate::config::Config,
    pub request_headers: &'a std::collections::BTreeMap<String, String>,
    pub stream: bool,
    pub timeout_ms: u64,
    pub model: &'a str,
    pub structured: bool,
}

pub fn build_request(
    provider: &crate::types::ProviderId,
    account: &crate::types::AvailableAccount,
    body: serde_json::Value,
    ctx: &RequestCtx<'_>,
) -> anyhow::Result<UpstreamRequest> {
    match provider.kind {
        crate::types::ProviderKind::Anthropic => anthropic::build_request(provider, account, body, ctx),
        crate::types::ProviderKind::Codex     => codex::build_request(provider, account, body, ctx),
        crate::types::ProviderKind::Opencode  => opencode::build_request(provider, account, body, ctx),
    }
}
```

In each per-kind module, expose `build_request(provider, account, body, ctx) -> Result<UpstreamRequest>`. The body of each is what was previously inlined in `app.rs` for that provider — extract it.

- [ ] **Step 3: Replace app.rs call sites**

In `src/app.rs`, every place that constructed a URL + headers per provider now calls `crate::upstream::build_request(provider, account, body, &ctx)`. The result is fed straight into the existing reqwest client.

- [ ] **Step 4: Run quality gates**

```bash
cargo fmt --check && cargo test --locked && cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS. Same behavior, factored differently.

- [ ] **Step 5: Commit**

```bash
git add src/upstream/ src/app.rs
git commit -m "add upstream::build_request dispatcher"
```

---

## Task 14: Split `translate.rs` into per-format modules

**Files:**
- Create: `src/translate/mod.rs`
- Create: `src/translate/anthropic.rs`
- Create: `src/translate/openai_chat.rs`
- Create: `src/translate/openai_responses.rs`
- Delete: `src/translate.rs`

- [ ] **Step 1: Move code verbatim**

This is a pure code-move. Group by upstream format:

- `anthropic.rs` — every fn dealing with the Anthropic Messages JSON schema.
- `openai_chat.rs` — passthrough helpers + chat-completions normalization.
- `openai_responses.rs` — the Responses-API translation for Codex.

Each module exposes `pub fn to_upstream(body: Value) -> Result<Value>` and `pub fn from_upstream(body: Value) -> Result<Value>`. For `openai_chat`, both are passthrough (`Ok(body)`); the file exists so the dispatcher has somewhere to point.

`src/translate/mod.rs`:

```rust
pub mod anthropic;
pub mod openai_chat;
pub mod openai_responses;

pub fn to_upstream(provider: &crate::types::ProviderId, body: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    match provider.kind {
        crate::types::ProviderKind::Anthropic => anthropic::to_upstream(body),
        crate::types::ProviderKind::Codex     => openai_responses::to_upstream(body),
        crate::types::ProviderKind::Opencode  => openai_chat::to_upstream(body),
    }
}

pub fn from_upstream(provider: &crate::types::ProviderId, body: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    match provider.kind {
        crate::types::ProviderKind::Anthropic => anthropic::from_upstream(body),
        crate::types::ProviderKind::Codex     => openai_responses::from_upstream(body),
        crate::types::ProviderKind::Opencode  => openai_chat::from_upstream(body),
    }
}
```

- [ ] **Step 2: Update import sites**

Update `src/app.rs` and any other importer to use the new module paths and dispatchers.

- [ ] **Step 3: Run quality gates**

```bash
cargo fmt --check && cargo test --locked && cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "split translate.rs into per-format modules"
```

---

## Task 15: Split `streaming.rs` into per-format modules

**Files:**
- Create: `src/streaming/mod.rs`
- Create: `src/streaming/anthropic.rs`
- Create: `src/streaming/openai_chat.rs`
- Create: `src/streaming/openai_responses.rs`
- Delete: `src/streaming.rs`

- [ ] **Step 1: Move code verbatim**

Same shape as Task 14 — group by upstream SSE shape. Each module exposes whatever streaming helpers it owned in the original file. `mod.rs` adds a dispatcher only if `app.rs` needs one; if streaming code is already accessed via `crate::streaming::<fn>` directly, the dispatcher is unnecessary and we can rely on path-based dispatch from per-kind helpers.

- [ ] **Step 2: Update import sites**

Search-replace.

- [ ] **Step 3: Quality gates**

```bash
cargo fmt --check && cargo test --locked && cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "split streaming.rs into per-format modules"
```

---

## Task 16: Split `oauth.rs` and introduce `OAuthProvider` trait

**Files:**
- Create: `src/oauth/mod.rs`
- Create: `src/oauth/flow.rs`
- Create: `src/oauth/anthropic.rs`
- Create: `src/oauth/codex.rs`
- Delete: `src/oauth.rs`

- [ ] **Step 1: Extract the shared types**

`src/oauth/flow.rs`:

```rust
use std::collections::BTreeMap;
use std::pin::Pin;

use anyhow::Result;
use serde_json::Value;
use url::Url;

use crate::types::{PkceCodes, TokenData};

#[derive(Debug, Clone, Copy)]
pub enum OAuthFlow {
    AuthorizationCodePkce,
    DeviceCode,
    CustomCallback,
}

#[derive(Debug, Clone)]
pub enum RedirectStyle {
    LocalhostFreePort,
    FixedPort { port: u16, path: &'static str },
    Manual,
}

#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub client_id: &'static str,
    pub authorize_url: Url,
    pub token_url: Url,
    pub scopes: Vec<&'static str>,
    pub flow: OAuthFlow,
    pub redirect: RedirectStyle,
    pub callback_path: &'static str,
    pub extra_authorize_params: BTreeMap<&'static str, String>,
}

pub type RefreshFuture = Pin<Box<dyn std::future::Future<Output = Result<TokenData>> + Send>>;

pub trait OAuthProvider: Send + Sync {
    fn config(&self) -> &OAuthConfig;
    fn authorize_url(&self, state: &str, pkce: &PkceCodes) -> String;
    fn exchange_code(
        &self,
        code: &str,
        callback_state: &str,
        expected_state: &str,
        pkce: &PkceCodes,
    ) -> RefreshFuture;
    fn refresh(&self, refresh_token: &str) -> RefreshFuture;
    fn map_tokens_from_callback(&self, raw: Value, hint: Option<&str>) -> Result<TokenData>;
}
```

- [ ] **Step 2: Move per-provider impls**

Create `src/oauth/anthropic.rs` and move anthropic OAuth code (existing `generate_anthropic_auth_url`, `exchange_anthropic_code`, refresh) into an impl of `OAuthProvider` for a new `struct AnthropicOAuth;`.

Same for `src/oauth/codex.rs` with `struct CodexOAuth;`.

Constants like `ANTHROPIC_REDIRECT_URI`, `CODEX_CALLBACK_PORT`, `CODEX_CALLBACK_PATH` live on the per-provider modules; expose them via `pub const` if used elsewhere.

- [ ] **Step 3: Re-export from `mod.rs`**

```rust
pub mod flow;
pub mod anthropic;
pub mod codex;

pub use flow::{OAuthConfig, OAuthFlow, OAuthProvider, RedirectStyle};

#[must_use]
pub fn provider_for(kind: crate::types::ProviderKind) -> Option<Box<dyn OAuthProvider>> {
    match kind {
        crate::types::ProviderKind::Anthropic => Some(Box::new(anthropic::AnthropicOAuth)),
        crate::types::ProviderKind::Codex     => Some(Box::new(codex::CodexOAuth)),
        crate::types::ProviderKind::Opencode  => None,  // static key, no OAuth
    }
}
```

- [ ] **Step 4: Update `runtime.rs::login`**

Replace the `match provider { ProviderId::Anthropic => ... }` block with:

```rust
let Some(oauth) = crate::oauth::provider_for(provider.kind) else {
    return save_opencode_login(config, key);   // only opencode has no OAuth in phase 0
};
let auth_url = oauth.authorize_url(&state, &pkce);
// ...
let token = self.runtime.block_on(oauth.exchange_code(&callback.code, &callback.state, &state, &pkce))?;
```

- [ ] **Step 5: Quality gates**

```bash
cargo fmt --check && cargo test --locked && cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "split oauth.rs and introduce OAuthProvider trait"
```

---

## Task 17: CLI `--provider` takes id string

**Files:**
- Modify: `src/cli.rs` (the `Login` subcommand)
- Modify: `src/runtime.rs::login` signature

- [ ] **Step 1: Write the failing test**

Add to `src/cli.rs::tests`:

```rust
#[test]
fn login_validates_provider_against_registry() {
    let providers = vec![
        crate::config::ProviderConfig { id: "anthropic".into(), kind: ProviderKind::Anthropic },
        crate::config::ProviderConfig { id: "codex".into(),     kind: ProviderKind::Codex },
    ];
    let registry = crate::providers::ProviderRegistry::from_config(&providers);
    assert!(registry.by_id("anthropic").is_some());
    assert!(registry.by_id("groq").is_none());
}
```

(Most CLI plumbing is integration-tested via end-to-end, but this asserts the lookup primitive.)

- [ ] **Step 2: Change the CLI argument**

In `src/cli.rs`, the `Login` subcommand currently has `provider: ProviderId` parsed by `FromStr`. Change it to `provider: String` and resolve via the registry at run time:

```rust
let Some(provider) = registry.by_id(&login_args.provider) else {
    bail!("unknown provider id '{}'; configured: {}",
          login_args.provider,
          registry.all().iter().map(|p| &*p.id).collect::<Vec<_>>().join(", "));
};
runtime.login(config, provider.clone(), login_args.manual, login_args.key.as_deref())?;
```

Update `CliRuntime::login` signature accordingly: `fn login(&mut self, config: &Config, provider: ProviderId, manual: bool, key: Option<&str>) -> Result<String>`. `ProviderId` (the struct) replaces the old enum copy.

- [ ] **Step 3: Run quality gates**

```bash
cargo fmt --check && cargo test --locked && cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/cli.rs src/runtime.rs
git commit -m "resolve --provider by registry id"
```

---

## Task 18: `pengepul accounts` lists providers by id

**Files:**
- Modify: `src/app.rs` (the `/admin/accounts` handler and its CLI mirror)

- [ ] **Step 1: Find current output shape**

```bash
rg -n '"accounts"' src/app.rs
```

Today's response shape (per `accounts.rs::snapshots`) is per-provider JSON arrays. Find the place that combines them.

- [ ] **Step 2: Change the response shape**

`/admin/accounts` returns:

```json
{
  "providers": [
    {
      "id": "anthropic",
      "kind": "anthropic",
      "accounts": [ /* AccountManager::snapshots() */ ]
    },
    { "id": "codex", "kind": "codex", "accounts": [...] },
    { "id": "opencode", "kind": "opencode", "accounts": [...] }
  ]
}
```

Build it by iterating `state.account_managers` (HashMap from Task 10).

- [ ] **Step 3: Run quality gates**

```bash
cargo fmt --check && cargo test --locked && cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/app.rs
git commit -m "include provider id+kind in /admin/accounts response"
```

---

## Task 19: README breaking-change notes

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add a top-level "Breaking changes (0.2)" section**

Right after the `## About` section in `README.md`, insert:

```markdown
## Breaking changes in 0.2

`pengepul 0.2` introduces multi-provider support. Three things change for existing clients.

### Model prefix is now mandatory

Every request to `/v1/chat/completions`, `/v1/messages`, `/v1/messages/count_tokens`, and `/v1/responses` must include a provider prefix in the `model` field.

| Before (0.1)              | After (0.2)                              |
|---------------------------|------------------------------------------|
| `claude-sonnet-4-6`       | `anthropic/claude-sonnet-4-6`            |
| `claude-haiku-4-5`        | `anthropic/claude-haiku-4-5`             |
| `gpt-5.5`                 | `codex/gpt-5.5`                          |
| `gpt-5.4`                 | `codex/gpt-5.4`                          |
| `opencode/glm-5.1`        | `opencode/glm-5.1` *(unchanged)*         |

Update your Claude Code / Codex CLI configuration to send the prefixed model name.

### Token storage moves to per-provider subdirectories

`~/.pengepul/claude-<email>.json` is now `~/.pengepul/anthropic/<email>.json`. Migration runs once automatically on the first start of `pengepul 0.2`. No action required.

### `pengepul login --provider <id>` takes a string id

The closed enum (`anthropic | codex | opencode`) is replaced by configured provider ids from `~/.pengepul/config.yaml`. The default config still lists `anthropic`, `codex`, and `opencode`, so existing login commands continue to work.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "document 0.2 breaking changes in README"
```

---

## Task 20: Cargo version bump

**Files:**
- Modify: `Cargo.toml:3`

- [ ] **Step 1: Bump version**

In `Cargo.toml`, change:

```toml
version = "0.1.0"
```

to:

```toml
version = "0.2.0"
```

- [ ] **Step 2: Refresh `Cargo.lock`**

```bash
cargo build --locked --offline 2>/dev/null || cargo update -p pengepul
```

(`-p pengepul` updates only our crate's entry in the lockfile, leaving dependencies pinned.)

- [ ] **Step 3: Run quality gates**

```bash
cargo fmt --check && cargo test --locked && cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "bump version to 0.2.0"
```

---

## Task 21: Acceptance gate

**Files:** none — verification only.

- [ ] **Step 1: Run the full quality gate**

```bash
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
```

All three must pass.

- [ ] **Step 2: Storage migration smoke test**

Use a throwaway directory to verify migration handles a realistic legacy layout:

```bash
TMP=$(mktemp -d)
mkdir -p "$TMP"
cat > "$TMP/claude-alice_at_x.com.json" <<EOF
{"access_token":"a","refresh_token":"r","expired":"2099-01-01T00:00:00Z","type":"claude","email":"alice@x.com"}
EOF
cat > "$TMP/codex-bob_at_y.com.json" <<EOF
{"access_token":"a","refresh_token":"r","expired":"2099-01-01T00:00:00Z","type":"codex","email":"bob@y.com"}
EOF
cat > "$TMP/opencode-deadbeef.json" <<EOF
{"access_token":"sk","refresh_token":"","expired":"9999-12-31T23:59:59Z","type":"opencode","email":"opencode-deadbeef"}
EOF

cargo run --locked --quiet -- accounts --reload 2>&1 || true   # exits non-zero if no server; that's fine for layout check

ls "$TMP/anthropic/" "$TMP/codex/" "$TMP/opencode/"
```

Confirm the three new directories exist and the top-level files are gone (or at least empty).

- [ ] **Step 3: Manual login flow check (live network — gate before tagging)**

In a separate clean `HOME`, run:

```bash
pengepul login --provider anthropic
pengepul login --provider codex
pengepul login --provider opencode
```

Each must complete; afterwards `pengepul accounts` must list one account per provider with kind populated.

- [ ] **Step 4: Manual `/v1/models` shape check**

Start `pengepul serve` against the test HOME and:

```bash
API_KEY=$(awk '/api-keys:/{getline; sub(/^[[:space:]]*-[[:space:]]*/, ""); print; exit}' ~/.pengepul/config.yaml)
curl -sS http://127.0.0.1:8317/v1/models -H "Authorization: Bearer $API_KEY" | jq '.data[].id'
```

Every id must contain a `/`. Specifically expect entries starting with `anthropic/`, `codex/`, `opencode/`.

- [ ] **Step 5: Manual prefix-required check**

```bash
curl -sS http://127.0.0.1:8317/v1/chat/completions \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"glm-5.1","messages":[{"role":"user","content":"hi"}]}'
```

Must return HTTP 400 with the `"model id must include a provider prefix"` error message.

```bash
curl -sS http://127.0.0.1:8317/v1/chat/completions \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"opencode/glm-5.1","messages":[{"role":"user","content":"reply: pong"}]}'
```

Must return a normal chat completion.

- [ ] **Step 6: Tag**

If every step above passes:

```bash
git tag v0.2.0
```

(Push the tag only after the user approves.)

---

## Self-review notes (for the engineer reading this plan)

A few common traps that the spec doesn't restate but matter here:

1. **`ProviderId` is no longer `Copy`.** Every place that took `ProviderId` by value used to be cheap. Now it's `clone()` (cheap due to `Arc`, but the compiler will flag every move). Prefer `&ProviderId` in signatures where you can.
2. **`AccountManager::load(&mut self)` calls `load_all_tokens(&self.auth_dir, Some(self.provider))`.** After Task 10, `self.provider` is the struct, so call `Some(&self.provider)` and update the `load_all_tokens` signature accordingly.
3. **Concurrency around `account_managers: HashMap<ProviderId, AccountManager>`.** The current `app.rs` likely wraps each per-provider manager in `Mutex<...>` or `tokio::sync::Mutex<...>`. Keep that wrapper inside the HashMap value (`HashMap<ProviderId, Mutex<AccountManager>>`). Don't introduce a single Mutex around the whole map — that would serialize all provider traffic.
4. **`storage_to_token` round-trip from legacy files.** Legacy `type: "claude"` files must still parse to `ProviderId::anthropic()` (kind: Anthropic, id: "anthropic"). That's what makes the migration transparent to users.
5. **clippy pedantic.** This crate enables `clippy::pedantic`. Common pedantic hits: `module_name_repetitions` (rename functions like `anthropic::anthropic_headers` → `anthropic::headers` when moving), `must_use_candidate` (add `#[must_use]` to new public functions).
6. **Deferred CLI commands.** The spec mentions `pengepul logout --provider <id>`, `pengepul config providers`, and `pengepul config providers --add <id>`. These are CLI ergonomics, not architectural — they're deferred until after Phase 0 ships. Phase 0 needs only `--provider <id>` taking a string (Task 17) and `pengepul accounts` showing the new shape (Task 18).
