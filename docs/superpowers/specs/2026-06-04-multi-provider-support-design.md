# Multi-provider support for pengepul

Status: design, awaiting review
Date: 2026-06-04
Owner: gitshrl
Reference: 9router (https://github.com/decolua/9router) for provider catalog + OAuth flow ground-truth

## Goal

Extend pengepul from 3 upstream providers (Anthropic, Codex, opencode) to roughly 30 by adding:

- 15 generic OpenAI-compatible API-key providers (groq, mistral, deepseek, glm, minimax, kimi, openrouter, perplexity, together, cerebras, fireworks, hyperbolic, sambanova, openai-key, anthropic-key)
- 1 native Gemini provider
- 5 OAuth / cloud providers (Copilot, Cursor, Antigravity, Kiro, Vertex)

The architecture must let the long tail of API-key providers be added by editing YAML, not by writing Rust. The bespoke providers stay coded.

## Non-goals

- RTK-style input-token compression. Cross-cutting middleware; tracked in a separate spec.
- Fallback chains / "combos" (subscription → cheap → free). pengepul stays single-provider per request; the client picks the provider via the model prefix.
- Web dashboard / UI. pengepul stays CLI-only.
- Cloud config sync. Single-host.
- Multi-account weighting, quota tracking. Existing round-robin + exponential backoff is unchanged.
- The OAuth long tail from 9router (`qwen`, `iflow`, `qoder`, `xai`, `kimi-coding`, `kilocode`, `cline`, `codebuddy`). They fit the OAuth framework and can be added as small follow-up PRs without re-speccing.

## Breaking changes (suggested release bump: 0.2.0 or 1.0.0)

1. **Model prefix becomes mandatory on every route.** Bare model ids like `claude-sonnet-4-6` or `gpt-5.5` no longer route. Clients must send `anthropic/claude-sonnet-4-6`, `codex/gpt-5.5`, `groq/llama-3.3-70b`, etc. Affects `/v1/chat/completions`, `/v1/messages`, `/v1/messages/count_tokens`, `/v1/responses`.
2. **Storage layout moves under per-id subdirectories.** `~/.pengepul/claude_<email>.json` becomes `~/.pengepul/anthropic/<email>.json`. One-shot auto-migration runs on first start of the new version.
3. **`ProviderId` is no longer a closed enum.** Public APIs that accepted `ProviderId::Anthropic | Codex | Opencode` now take a `ProviderId { kind, id }` struct.

Mitigation for (1): release notes call out the change with copy-pasteable model-id remaps. Tooling that wraps pengepul (custom Claude Code wrappers, etc.) needs to be updated.

## Architecture

### `ProviderKind` and `ProviderId`

```rust
// src/types.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    Anthropic,          // OAuth (existing)
    AnthropicKey,       // API key
    Codex,              // OAuth (existing)
    Opencode,           // static key (existing)
    GenericOpenAi,      // OpenAI chat/completions; many YAML instances
    Gemini,             // Google native format, OAuth or API key
    Vertex,             // GCP service account JWT
    Copilot,            // GitHub OAuth + copilot-internal token exchange
    Cursor,             // OAuth
    Antigravity,        // PKCE OAuth
    Kiro,               // PKCE OAuth
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderId {
    pub kind: ProviderKind,
    pub id: Arc<str>,   // routing prefix and on-disk dir name
}
```

`kind` drives behavior (header builder, OAuth flow, translator); `id` distinguishes instances of the same kind and is the user-facing string in model prefixes and storage paths.

Only `GenericOpenAi` ever has more than one `id`. Every other kind has exactly one canonical id (the same string as the kind name, lowercased).

### `ProviderRegistry`

Becomes config-driven. Built at `Runtime::new` from:

1. The list of bespoke kinds present in `config.yaml::providers[]`.
2. Defaults for each bespoke kind (base URL, model regex, capabilities).
3. The `generic-openai` entries from YAML (base URL + model list per entry).

`ProviderRegistry::route(model: &str) -> Option<&ProviderId>` looks up by explicit prefix. There is no implicit/regex fallback — prefix is mandatory.

### Routing

```rust
pub fn route(&self, model: &str) -> Option<&ProviderId> {
    let (prefix, _) = model.split_once('/')?;
    self.by_id.get(prefix)
}
```

`/v1/models` returns the union of every loaded provider's models, namespaced with the provider id:

```json
{ "data": [
  { "id": "anthropic/claude-sonnet-4-6", "owned_by": "anthropic" },
  { "id": "codex/gpt-5.5",               "owned_by": "codex" },
  { "id": "opencode/glm-5.1",            "owned_by": "opencode" },
  { "id": "groq/llama-3.3-70b",          "owned_by": "groq" },
  { "id": "copilot/gpt-5.4",             "owned_by": "copilot" },
  { "id": "gemini/gemini-3.1-pro",       "owned_by": "gemini" }
] }
```

Generic providers with `models: []` (or unset) lazy-fetch their upstream `/v1/models` on first call and cache for 1h in-process.

The body sent upstream has the prefix stripped (`groq/llama-3.3-70b` → `llama-3.3-70b`). One shared helper `strip_provider_prefix(model: &str) -> &str` replaces today's `strip_opencode_prefix`.

## Configuration

### YAML schema

```yaml
host: ""
port: 8317
auth-dir: ~/.pengepul
api-keys: [sk-local-example]
body-limit: 200mb
timeouts:
  messages-ms: 120000
  stream-messages-ms: 600000
  count-tokens-ms: 30000
stats:
  enabled: true
debug: off

# NEW
providers:
  # Bespoke kinds. Opt in by listing; behavior lives in code.
  - id: anthropic
    kind: anthropic
  - id: anthropic-key
    kind: anthropic-key
  - id: codex
    kind: codex
  - id: opencode
    kind: opencode
  - id: gemini
    kind: gemini
  - id: vertex
    kind: vertex
    project: my-gcp-project
    region: us-central1
  - id: copilot
    kind: copilot
  - id: cursor
    kind: cursor
  - id: antigravity
    kind: antigravity
  - id: kiro
    kind: kiro

  # Generic OpenAI-compatible. Pure data.
  - id: groq
    kind: generic-openai
    base-url: https://api.groq.com/openai/v1
    models: [llama-3.3-70b, llama-3.1-8b-instant, mixtral-8x7b]
  - id: mistral
    kind: generic-openai
    base-url: https://api.mistral.ai/v1
    models: []                     # lazy /v1/models discovery
  - id: deepseek
    kind: generic-openai
    base-url: https://api.deepseek.com/v1
    models: [deepseek-chat, deepseek-reasoner]
  # ... glm, minimax, kimi, openrouter, perplexity, together, cerebras,
  #     fireworks, hyperbolic, sambanova, openai-key
```

Compatibility rules:

- Existing configs without a `providers:` section keep working — pengepul defaults to `[anthropic, codex, opencode]` (the prior 3) so users who never edit the file see no behavior change other than the prefix-required breaking change.
- A `providers:` section is treated as the complete list. Listed entries with no credentials show up in `pengepul accounts` as "configured, no accounts loaded."
- `cloaking:` stays anthropic/codex-only and is ignored by every other kind.

### Storage layout

Before (filenames use hyphen separator, sanitized email; see `src/tokens.rs:35`):

```
~/.pengepul/
├── config.yaml
├── claude-alice_at_example.com.json
├── codex-alice_at_example.com.json
└── opencode-opencode-a1b2c3d4.json
```

After:

```
~/.pengepul/
├── config.yaml
├── anthropic/alice_at_example.com.json
├── anthropic-key/personal.json
├── codex/alice_at_example.com.json
├── opencode/opencode-a1b2c3d4.json
├── groq/personal.json
├── mistral/work.json
├── gemini/alice_at_gmail.com.json
├── copilot/alice_at_github.com.json
└── ...
```

Migration runs once on startup: any top-level file matching `<legacy-prefix>-<rest>.json` (where `legacy-prefix` ∈ {`claude`, `codex`, `opencode`}) is moved to `<id>/<rest>.json` (mapping `claude` → `anthropic`, `codex` → `codex`, `opencode` → `opencode`). Idempotent. Logged at info level.

File shapes:

- OAuth provider files keep today's `TokenData` JSON (access/refresh tokens, expiry, account uuid, email).
- API-key provider files: `{ "kind": "generic-openai", "key": "...", "account": "personal", "added_at": "2026-06-04T..." }`. Account label defaults to the filename stem; user can override with `--account`.
- Vertex service-account: `{ "kind": "vertex", "service_account_json": { ... }, "account": "..." }`.

## Per-kind behavior

### Upstream HTTP (`src/upstream/`)

`upstream.rs` is split into:

```
src/upstream/
├── mod.rs              // re-exports + dispatcher
├── shared.rs           // header conventions, base utilities
├── anthropic.rs        // existing code, moved
├── codex.rs            // existing code, moved
├── opencode.rs         // existing code, moved
├── anthropic_key.rs    // new: x-api-key, no cloaking
├── generic_openai.rs   // new: bearer + Content-Type + Accept
├── gemini.rs           // new: x-goog-api-key or Bearer; base URL per region
├── vertex.rs           // new: bearer with signed-JWT; regional endpoint
├── copilot.rs          // new: copilot-internal/v2/token then bearer
├── cursor.rs           // new
├── antigravity.rs      // new
└── kiro.rs             // new
```

Dispatcher signature:

```rust
pub fn build_request(
    provider: &ProviderId,
    account: &AvailableAccount,
    body: serde_json::Value,
    ctx: &RequestCtx,
) -> Result<UpstreamRequest> {
    match provider.kind {
        ProviderKind::Anthropic     => anthropic::build_request(provider, account, body, ctx),
        ProviderKind::AnthropicKey  => anthropic_key::build_request(provider, account, body, ctx),
        ProviderKind::Codex         => codex::build_request(provider, account, body, ctx),
        ProviderKind::Opencode      => opencode::build_request(provider, account, body, ctx),
        ProviderKind::GenericOpenAi => generic_openai::build_request(provider, account, body, ctx),
        ProviderKind::Gemini        => gemini::build_request(provider, account, body, ctx),
        ProviderKind::Vertex        => vertex::build_request(provider, account, body, ctx),
        ProviderKind::Copilot       => copilot::build_request(provider, account, body, ctx),
        ProviderKind::Cursor        => cursor::build_request(provider, account, body, ctx),
        ProviderKind::Antigravity   => antigravity::build_request(provider, account, body, ctx),
        ProviderKind::Kiro          => kiro::build_request(provider, account, body, ctx),
    }
}
```

Cloaking (`apply_cloaking`, `build_beta_header`, `codex_user_agent`) stays scoped to Anthropic and Codex kinds.

### OAuth (`src/oauth/`)

```
src/oauth/
├── mod.rs              // shared driver: local HTTP callback, PKCE, --manual fallback
├── flow.rs             // OAuthConfig, OAuthFlow enum, RedirectStyle enum, trait
├── anthropic.rs        // existing
├── codex.rs            // existing
├── gemini.rs           // new
├── vertex.rs           // new (service-account JWT — not really OAuth, lives here for proximity)
├── copilot.rs          // new (GitHub device-code + token-exchange)
├── cursor.rs           // new
├── antigravity.rs      // new
└── kiro.rs             // new
```

Shared types:

```rust
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

pub enum OAuthFlow { AuthorizationCodePkce, DeviceCode, CustomCallback }
pub enum RedirectStyle { LocalhostFreePort, FixedPort(u16), Manual }

pub trait OAuthProvider {
    fn config(&self) -> &OAuthConfig;
    fn map_tokens(&self, raw: serde_json::Value, hint: Option<&str>) -> Result<TokenData>;
    fn refresh(&self, refresh_token: &str) -> RefreshFuture;
    fn post_exchange(&self, _tokens: &mut TokenData) -> BoxFuture<'_, Result<()>> {
        // default: no-op
        Box::pin(async { Ok(()) })
    }
}
```

Most providers use `AuthorizationCodePkce + LocalhostFreePort` and only differ in client_id, URLs, scopes, and `map_tokens`. The exotic ones:

- **Codex** — `FixedPort(1455)`, PKCE; existing code stays.
- **Copilot** — `DeviceCode`; after the GitHub access token is obtained, `post_exchange` calls `https://api.github.com/copilot_internal/v2/token` to get the runtime token used for chat. Refresh policy `SinceLastRefresh` every ~25min.
- **Cursor** — custom callback URL (cursor.com hands the token via a deep link), needs `CustomCallback` flow with paste-as-fallback.
- **Vertex** — not OAuth at all; refresh is signed-JWT → access-token exchange against `https://oauth2.googleapis.com/token`. Refresh policy is a new `RefreshPolicyKind::ServiceAccountJwt` (refresh every 50min).

**Ground truth.** Per-provider OAuth client IDs, URLs, scopes, and token-shape mappings are not invented here — they live in 9router's repo under `src/lib/oauth/constants/` and `src/lib/oauth/services/<name>.js`. Each per-provider module in pengepul cites the matching 9router file at the top. During implementation, that JS is the source of truth; verification means running the login flow once against the real upstream before merging the phase.

### Format translation (`src/translate/`)

`translate.rs` is split into:

```
src/translate/
├── mod.rs              // dispatcher
├── anthropic.rs        // existing translation, moved
├── openai_chat.rs      // existing, moved
├── openai_responses.rs // existing, moved
└── gemini.rs           // new: openai-chat <-> gemini generate-content
```

Dispatcher:

```rust
pub fn to_upstream(provider: &ProviderId, body: Value) -> Result<Value> {
    match provider.kind {
        ProviderKind::Anthropic | ProviderKind::AnthropicKey => anthropic::from_openai(body),
        ProviderKind::Codex                                   => openai_responses::from_openai_chat(body),
        ProviderKind::Gemini | ProviderKind::Vertex           => gemini::from_openai(body),
        // all OpenAI-chat-shaped providers pass through:
        ProviderKind::Opencode | ProviderKind::GenericOpenAi
        | ProviderKind::Copilot | ProviderKind::Cursor
        | ProviderKind::Antigravity | ProviderKind::Kiro      => Ok(body),
    }
}

pub fn from_upstream(provider: &ProviderId, body: Value) -> Result<Value> { /* mirror */ }
```

Gemini's format (excerpt for context):

```json
{
  "contents": [
    { "role": "user", "parts": [{ "text": "hello" }] }
  ],
  "tools": [
    { "function_declarations": [{ "name": "...", "parameters": { ... } }] }
  ],
  "generationConfig": { "maxOutputTokens": 1024, "temperature": 0.7 },
  "systemInstruction": { "parts": [{ "text": "..." }] }
}
```

Maps to/from OpenAI chat messages on translation.

### Streaming (`src/streaming/`)

```
src/streaming/
├── mod.rs              // dispatcher
├── anthropic.rs        // existing SSE <-> openai chunks
├── openai_chat.rs      // existing
├── openai_responses.rs // existing
└── gemini.rs           // new: Gemini SSE <-> openai chunks
```

OAuth-non-Anthropic-non-Codex providers all speak OpenAI SSE and reuse existing code unchanged.

## Account selection and rotation

`accounts.rs` is already provider-agnostic for the parts that matter (rotation, backoff, refresh policies). Required changes:

- `AccountManager` is keyed on `ProviderId` (struct) not the old enum copy. `Runtime` holds `HashMap<ProviderId, AccountManager>`.
- Add `RefreshPolicyKind::ServiceAccountJwt` for Vertex.
- Per-kind `record_failure` classification: `upstream.rs` maps HTTP status → kind string (`"auth"` for 401/403, `"rate-limit"` for 429, `"upstream-5xx"` for 5xx, `"upstream-timeout"` for network errors). Same backoff curve applies to all of them, so user-visible behavior is consistent across providers.

| Kind | Refresh policy |
|---|---|
| Anthropic, Codex, Gemini, Cursor, Antigravity, Kiro | `ExpiresLead` (existing) |
| AnthropicKey, GenericOpenAi, Opencode | `Never` (existing) |
| Copilot | `SinceLastRefresh` (~25min) |
| Vertex | `ServiceAccountJwt` (new — 50min) |

## CLI surface

```text
pengepul login --provider <id> [--key <api-key>] [--account <label>] [--manual]
pengepul logout --provider <id> [--account <label>]
pengepul accounts [--provider <id>] [--reload]
pengepul config providers                              # list providers from YAML
pengepul config providers --add <id>                   # interactive add for known kinds
```

- `--provider <id>` accepts any id in the registry (validated against `config.yaml::providers[].id`). The old enum-based validation is replaced by a string lookup.
- `--key` is accepted only for kinds whose auth is a static key (`AnthropicKey`, `GenericOpenAi`, `Opencode`). For OAuth kinds, `--key` is rejected with a clear message pointing at the OAuth flow.
- `--account <label>` is the filename stem under `~/.pengepul/<id>/`. Defaults: OAuth uses the token's `email`; API-key uses `"default"`.
- `pengepul help` lists kinds and shows `pengepul login --provider <id>` examples for each.

## Testing

| Layer | What's tested | Where |
|---|---|---|
| Routing | one test per provider id; prefix lookup correctness; rejection of bare ids | `src/providers.rs::tests` |
| Account rotation | unchanged from today; uses a fake `RefreshFn` | `src/accounts.rs::tests` |
| Header builders | one test per kind asserting the exact header set | `src/upstream/<kind>.rs::tests` |
| Translation | round-trip tests per format pair | `src/translate/<format>.rs::tests` |
| Streaming | event-by-event golden tests per format | `src/streaming/<format>.rs::tests` |
| OAuth flows | mocked token-endpoint + PKCE verification | `src/oauth/<provider>.rs::tests` |
| End-to-end | stub upstream server records what pengepul sent; one test per kind | `tests/multi_provider.rs` (new) |

CI must continue to pass:

```bash
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
```

No live network in CI. Per-provider live verification is a manual gate before each phase ships.

## Phases

Phases ship as separate PRs. Phase 0 is mandatory groundwork; Phases 1–6 can be reordered or parallelized.

### Phase 0 — Foundation refactor (one PR)

- Introduce `ProviderKind` and `ProviderId { kind, id }`.
- Replace closed-enum sites with struct equality / `.kind` comparison.
- Move `upstream.rs` → `src/upstream/<kind>.rs` modules; add dispatcher.
- Move `translate.rs` and `streaming.rs` similarly.
- Move `oauth.rs` → `src/oauth/<provider>.rs`; add shared driver, `OAuthConfig`, `OAuthProvider` trait, `OAuthFlow`/`RedirectStyle` enums.
- Make prefix mandatory in routing. Strip prefix before sending body upstream.
- Switch `Runtime` to `HashMap<ProviderId, AccountManager>`.
- Add `providers:` block to config schema. Defaults preserve existing 3 providers.
- Implement per-id storage migration on startup.
- Update CLI: `--provider` takes id string; validate against registry.
- Update `/v1/models` to emit namespaced ids.
- Update README breaking-change notes + remap table.

No new providers ship in this phase. The diff is large but mechanical and existing tests must continue to pass with prefix-amended model ids.

### Phase 1 — Generic OpenAI-compatible (one PR)

- New `ProviderKind::GenericOpenAi`.
- New `src/upstream/generic_openai.rs`: bearer auth, Content-Type, Accept, optional User-Agent override per provider.
- New file shape `{ "kind": "generic-openai", "key": "...", "account": "...", "added_at": "..." }`.
- YAML reader for `generic-openai` entries; validation (non-empty base URL, valid URL).
- Lazy `/v1/models` discovery with 1h in-process cache when `models: []`.
- `pengepul login --provider groq --key gsk_... --account personal` works.
- Add 15 providers to the default config template: groq, mistral, deepseek, glm, minimax, kimi, openrouter, perplexity, together, cerebras, fireworks, hyperbolic, sambanova, openai-key, anthropic-key.
- Per-provider header tests + one end-to-end stub-upstream test.

### Phase 2 — Gemini native (one PR)

- New `ProviderKind::Gemini`.
- New `src/upstream/gemini.rs` (URL: `https://generativelanguage.googleapis.com/v1beta`).
- New `src/oauth/gemini.rs` (Google OAuth; PKCE + localhost callback). Reference: 9router `src/lib/oauth/services/gemini.js`, `src/lib/oauth/constants/oauth/gemini.js`.
- New `src/translate/gemini.rs` (openai-chat ↔ generate-content).
- New `src/streaming/gemini.rs` (Gemini SSE ↔ openai chunks).
- Golden translation tests, golden SSE tests.

### Phase 3 — GitHub Copilot (one PR)

- New `ProviderKind::Copilot`.
- New `src/oauth/copilot.rs` (GitHub device-code flow + `copilot-internal/v2/token` exchange in `post_exchange`). Reference: 9router `src/lib/oauth/services/github.js`.
- New `src/upstream/copilot.rs` (bearer + Copilot-specific headers).
- Refresh policy `SinceLastRefresh` at ~25min.
- Per-flow OAuth test with mocked token endpoint; live verification gate.

### Phase 4 — Cursor (one PR)

- New `ProviderKind::Cursor`.
- New `src/oauth/cursor.rs` (custom callback). Reference: 9router `src/lib/oauth/services/cursor.js`.
- New `src/upstream/cursor.rs`.

### Phase 5 — Antigravity + Kiro (one PR or two)

- New `ProviderKind::Antigravity`, `ProviderKind::Kiro`.
- PKCE flows. Reference: 9router `src/lib/oauth/services/antigravity.js`, `src/lib/oauth/services/kiro.js`.
- New `src/upstream/antigravity.rs`, `src/upstream/kiro.rs`.

### Phase 6 — Vertex AI (one PR)

- New `ProviderKind::Vertex`.
- Service-account JWT signing → access-token exchange. New `RefreshPolicyKind::ServiceAccountJwt`.
- Regional endpoint URL construction from `project` + `region` config.
- Reference: 9router service-account handling (precise file TBD by implementer).

## Open questions

The following are known unknowns that need answers during implementation, not design. They are listed so the implementer doesn't get blindsided:

- **Copilot token TTL.** Confirmed at ~25min in 9router but GitHub may have changed it. Verify by examining a fresh token's response.
- **Cursor OAuth callback.** 9router's `cursor.js` is the only documentation; we need to read it carefully before estimating Phase 4.
- **Vertex regional fallback.** Some Gemini models are region-restricted. Behavior on a region mismatch is TBD — likely return upstream 404 untouched, with a clear pengepul log line.
- **Gemini API-key vs OAuth.** Both work upstream. Spec implements OAuth; allowing API-key on the same kind is a small follow-up (one config field, one auth branch).
- **`/v1/models` cache invalidation.** 1h fixed in-memory cache may be too long for Groq's frequently-changing catalog. If issues arise, expose a config knob; not in v1.

## Acceptance

Phase 0 is accepted when:

- `cargo fmt --check && cargo test --locked && cargo clippy --locked --all-targets --all-features -- -D warnings` all pass.
- Existing OAuth flows for anthropic and codex still log in and serve requests with the new prefixed model ids.
- Storage migration converts a real legacy `~/.pengepul/` directory in one pass and is idempotent on re-run.
- `pengepul accounts` lists the three existing providers under their new ids.

Each later phase is accepted when:

- Its provider can be `login`-ed via CLI.
- `/v1/models` lists at least one of its models.
- A simple `pong` chat completion goes through end-to-end against the real upstream (manual gate; one engineer signs off).
- All CI gates pass.
