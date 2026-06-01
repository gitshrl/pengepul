# Cursor provider support

Status: approved design, pre-implementation (refined via a grilling pass on 2026-06-01)
Date: 2026-06-01
References:
- https://github.com/AmazingAng/auth2api — TypeScript implementation of the same feature (auth + chat).
- https://github.com/everestmz/cursor-rpc — extracted `aiserver.v1` `.proto` (older Cursor; used to
  confirm message/role shapes, not the unified-with-tools request layout).
- https://github.com/eisbaw/cursor_api_demo — Python reverse-engineering of the current
  `StreamUnifiedChatWithTools` path that auth2api itself follows.
- Cursor Composer 2.5 docs (verified the API SKU `composer-2.5`).

## Summary

Add Cursor as a fourth upstream provider in pengepul, focused on **Cursor's own Composer model**
(`composer-2.5`) — the proprietary model you cannot get from the Anthropic/Codex providers pengepul
already proxies. Cursor has no public API; access is reverse-engineered from the Cursor desktop
client. Unlike the existing three providers, Cursor does not speak OpenAI/Anthropic JSON over HTTPS
— it speaks **Connect-RPC over HTTP/2** with a hand-rolled protobuf body
(`application/connect+proto`). This design confines all of that reverse-engineering to two new
modules and makes Cursor behave, on the response side, exactly like the existing Codex provider
(OpenAI-Responses native format) so the entire client-format fan-out is reused unchanged.

The overriding constraint: **add only, never alter existing providers.** Every change is a new enum
variant, a new optional field, a new match arm, or a new module. The existing Anthropic/Codex/
OpenCodeGo code paths and on-disk token files are untouched, and all existing tests stay green.

## Goals

- **Composer-focused routing.** Models prefixed `cursor/` route to a Cursor account, mirroring the
  `opencode-go/` prefix. The prefix is stripped and the bare SKU is passed straight through to
  Cursor (`cursor/composer-2.5` → `composer-2.5`, `cursor/<any-sku>` → `<any-sku>`). Bare,
  unprefixed names keep routing to Anthropic/Codex exactly as today.
- **Two login methods** (both, matching auth2api):
  - **Browser deep-control login**: `pengepul login --provider cursor` prints a `cursor.com`
    URL, the user confirms in a browser, pengepul polls `api2.cursor.sh/auth/poll` until the token
    arrives.
  - **Local desktop import**: `pengepul login --provider cursor --cursor-import-local` reads the
    locally installed Cursor app's auth out of its SQLite store (`state.vscdb`).
- **Token refresh** against `api2.cursor.sh/oauth/token`, integrated with the existing
  `AccountManager` rotation/cooldown/refresh machinery.
- **Serve Cursor on all three client endpoints** (`/v1/chat/completions`, `/v1/messages`,
  `/v1/responses`) by reusing the existing Responses→{chat,anthropic} translators.
- **Convey the system prompt.** Like the other three providers (which carry it natively via
  Anthropic `system` / Codex `instructions` / chat `system` messages), Cursor must not silently drop
  it. See "System prompt handling" below.

## Non-goals (v1)

- **Text only.** No tool calls, no images, no structured output. The encoder/decoder handle
  user/assistant text + a reasoning channel — nothing else.
- **No re-exposure of Claude/GPT through Cursor.** The `PUBLIC_MODEL_TO_CURSOR` public-name→SKU
  translation table from the reference is intentionally **dropped** — re-serving models pengepul
  already proxies natively adds surface for no benefit. Routing is bare-SKU passthrough.
- **No structural system field.** The current `StreamUnifiedChatWithTools` request has no
  publicly-confirmed system/`explicit_context` field number, so v1 achieves *functional* parity by
  folding system text into the first user turn (below). Structural parity is a documented follow-up.
- **No live `AvailableModels` call.** `/v1/models` advertises a static `cursor/composer-2.5` (the
  SKU is verified). Other SKUs still work via `cursor/<sku>` passthrough.
- **No real token accounting.** Cursor does not return usage; per-account stats record
  requests/successes/failures but zero token counts.
- **No `count_tokens` support** for Cursor (returns 501, same as Codex/OpenCodeGo).
- **No "exclusive mode"** where Cursor serves unprefixed model names. Prefix-only routing.

## Architecture: Cursor as a Codex-shaped provider (Approach A)

pengepul reshapes upstream responses to the client's requested format in `app.rs`, keyed on
`(ProviderId, RequestRoute)`. Codex's native format is OpenAI-Responses, and the existing code
already converts Responses → chat (`responses_sse_to_chat`, `responses_to_chat_completion`),
Responses → anthropic (`responses_sse_to_anthropic`, `responses_to_anthropic_message`), and
Responses → Responses (passthrough).

Cursor's `native_format` is `"openai-responses"`. The Cursor upstream module decodes Cursor's
protobuf and emits **OpenAI-Responses SSE bytes** (stream) or a **Responses-API JSON object**
(non-stream) — the exact shape Codex's upstream already returns. Every reshaping site then groups
`ProviderId::Codex | ProviderId::Cursor`, reusing the fan-out verbatim. The only genuinely new logic
is the Cursor protocol (encode/decode/checksum/transport) and auth.

Rejected alternatives: (B) Cursor emits each client format directly (auth2api's `responseFormat`
switch) — reimplements translation pengepul already owns; (C) a new native format with dedicated
translators — most code, no benefit over A.

## System prompt handling

`ConversationMessage.MessageType` in Cursor's schema is only `HUMAN(1)`/`AI(2)` — there is **no
system role**. Cursor carries system/rules content in a separate `ExplicitContext.context` field on
the request, but that field's number on the *unified-with-tools* request is unconfirmed in any
public proto (the extracted proto covers the older `StreamChat`/`GetChatRequest` path only).
Guessing the field number risks the whole request being rejected.

v1 decision: **fold** the concatenated `system` text into the first user turn before encoding. The
model receives the instructions (functional parity); only the wire structure differs from native.
This is the only deviation from the reference, and it is deliberate — dropping system (what the
reference does) would make the relay near-useless for Claude-Code-style clients whose persona and
tool instructions live entirely in the system prompt.

Follow-up (out of scope): if someone extracts the unified request's `explicit_context` field number
(e.g. from `eisbaw/cursor_api_demo` at the current Cursor version), switch to true structural parity
by setting `explicit_context.context`. Isolated to the encoder, so it is a localized change later.

## Components

One new module does the protocol; one does auth; the rest is additive wiring that mirrors Codex.

### New module: `src/cursor.rs` — the Cursor wire protocol

Hand-rolled, no codegen. Pure functions where possible so they unit-test without a network.

Request encoding (ported from auth2api `src/upstream/cursor-api.ts`, which follows
`eisbaw/cursor_api_demo`):
- protobuf primitives: `encode_varint`, varint field (wire type 0), bytes/length-delimited field
  (wire type 2), message concat. ~80 lines of helpers. The magic field numbers for
  `StreamUnifiedChatRequestWithTools` live as named consts. **No `prost`/`build.rs`** — keeps the
  project's zero-codegen build and zero impact on the release/cross-compile pipeline.
- `encode_cursor_chat_request(body) -> (frame_bytes, conversation_id)`: builds the message
  (per-turn blocks, model message, cursor settings, metadata, message ids) and wraps it in a 5-byte
  Connect envelope `[flag u8][len u32 BE][payload]`.
- `messages_from_body(body)`: extract role/text from a Responses-shaped body (`input` string/array,
  `messages` array, top-level `system`). **System handling**: concatenate all `system` text and
  prepend it to the first user turn's content; do not emit a system role.
- model normalization: strip the `cursor/` prefix; default to `composer-2.5` when empty; otherwise
  pass the bare SKU through verbatim. (No translation table.)

Headers + auth:
- `cursor_headers(account, config)`: `Authorization: Bearer <jwt>`, `Content-Type`/`Accept:
  application/connect+proto`, `Connect-Protocol-Version: 1`, `x-cursor-checksum`,
  `x-cursor-client-version`, `x-cursor-config-version`, `x-session-id` (uuid v5 of the token),
  `x-client-key` (sha256 of token), and the static client/os/arch headers.
- `build_cursor_checksum(token, machine_id)`: port the `jyh` cipher over a timestamp-derived byte
  buffer, suffixed with the **stable machine id resolved as**
  `token.cursor.service_machine_id → account_uuid → device_id` (browser-login tokens have no
  `service_machine_id`, so they use the persisted `account_uuid`/pkce uuid — stable across restarts).

Transport:
- HTTP/2 POST to `https://api2.cursor.sh<CHAT_PATH>` via `reqwest`. reqwest negotiates h2 via ALPN
  over TLS (the `http2` feature is already enabled); response body read with `.bytes_stream()`.
- **New raw-bytes send helpers** (`send_bytes` / `send_bytes_stream`): the existing
  `send_json`/`send_stream`/`build_upstream_request` hard-code `.json(body)` →
  `Content-Type: application/json`, which Cursor's `application/connect+proto` body cannot use.
  These new helpers POST a `Vec<u8>` with a caller-supplied content type and apply the timeout.
- reqwest's own gzip feature stays **off**: Cursor gzips at the Connect-frame level, not the HTTP
  transport level, so frames are gunzipped manually with `flate2`.

Response decoding:
- `read_connect_frames(bytes)`: split the 5-byte-envelope frames; `flate2`-gunzip frames whose flag
  marks them compressed; leave others as-is.
- `extract_from_payload`: walk the (schemaless to us) protobuf, pull field-1 length-delimited
  strings as text and field-25 as reasoning, recursing into wrapper fields; skip uuid-like and
  non-printable leaves. Split on a literal `</think>` to route composer-style chain-of-thought out of
  the answer channel.
- `CursorStreamingDecoder`: stateful; `feed(chunk) -> Vec<{text_delta, reasoning_delta}>` parses the
  complete frames received so far; `finish() -> Option<error>`.
- error frames: parse the Connect end-of-stream JSON error; surface `code`/`message`.

Output adaptation (makes Cursor "Codex-shaped"):
- streaming: turn decoder deltas into OpenAI-Responses SSE byte chunks (`response.output_text.delta`,
  `response.reasoning_summary_text.delta`, terminal `response.completed`). The terminal
  `response.completed` is what the existing pipeline keys on to mark the stream complete and run
  success accounting.
- non-stream: accumulate all frames, decode fully, synthesize a Responses-API JSON object
  (`{id, object:"response", model, output:[...], usage: zeros}`) → `UpstreamJsonResponse`.

### New module: `src/cursor_auth.rs` — login + refresh

Ported from auth2api `src/auth/cursor/*`.

- Browser deep-control (no local callback server — Cursor has no redirect):
  - `generate_cursor_pkce()` → `{uuid, verifier, challenge}` (S256), reusing `utils.rs` helpers.
  - `build_cursor_login_url(pkce)` → `https://www.cursor.com/loginDeepControl?...&redirectTarget=cli`.
  - `poll_cursor_auth(uuid, verifier)`: GET `api2.cursor.sh/auth/poll?uuid&verifier` via reqwest
    (h2), backoff 1s→5s, 404/202 = pending, transient network errors = pending up to a cap, hard
    timeout ~5 min.
  - **Refuse a poll result with no refresh token** (PAT/api-key path): persisting the access token
    as the refresh token would push the account into permanent auth-failure ~1h later. Fail loudly;
    suggest retry or `--cursor-import-local`.
- Local import:
  - `default_cursor_storage_path()` per-OS, file `User/globalStorage/state.vscdb`.
  - read via **rusqlite (bundled)**: `SELECT key,value FROM {ItemTable,cursorDiskKV} WHERE key IN
    (...)` for the `cursorAuth/*` keys (accessToken, refreshToken, cachedEmail,
    stripeMembershipType, serviceMachineId, clientId, clientVersion, configVersion) → `TokenData`.
- Refresh: `refresh_cursor_tokens(refresh_token, previous) -> TokenData`:
  `POST api2.cursor.sh/oauth/token` JSON `{grant_type:"refresh_token", client_id, refresh_token}`;
  expiry from JWT `exp` (fallback +1h so `expires_at` always parses); map auth-exhaustion bodies /
  `shouldLogout` to `RefreshTokenExhaustedError`; guard against empty/echoed refresh tokens.

### `src/types.rs`

- `ProviderId::Cursor`: `Display` → `"cursor"`, `FromStr` accepts `"cursor"`, `storage_prefix()` →
  `"cursor"`.
- Extend `TokenData` with `cursor: Option<CursorMeta>` where
  `CursorMeta { service_machine_id: Option<String>, client_version: String, config_version: String,
  client_id: String }` (derives `PartialEq, Eq`). Reuse the existing `plan_type` field for Cursor's
  membership type. Other providers set `cursor: None`.

### `src/tokens.rs`

- `StoredToken`: add flat optional cursor fields, each
  `#[serde(default, skip_serializing_if = "Option::is_none")]` so existing Anthropic/Codex/OpenCodeGo
  token files are byte-for-byte unchanged on save.
- `token_to_storage` / `storage_to_token`: round-trip the cursor meta; map `token_type` `"cursor"`.
- `load_all_tokens`: add `"cursor-"` to the filename prefix allowlist (the `provider == None` branch).

### `src/accounts.rs`

- `refresh_account` rebuilds `TokenData` field-by-field; add
  `cursor: refreshed.cursor.or(old_token.cursor)` so machine id / client version / config version
  survive a refresh.
- No `AvailableAccount` change: it already embeds the full `token`, so `cursor.rs` reads
  `account.token.cursor` directly.

### `src/providers.rs`

- Register `Provider { id: Cursor, native_format: "openai-responses" }` in `build_registry` and the
  `get` fallback.
- `CURSOR_PREFIX = "cursor/"`, `cursor_matches_model` (prefix test), `strip_cursor_prefix`. Wire into
  `for_model` at the same position as the opencode-go prefix check.
- `CURSOR_MODELS: [&str; 1] = ["composer-2.5"]` — advertised on `/v1/models` as `cursor/composer-2.5`.
  (No translation map; routing accepts any `cursor/<sku>`.)

### `src/app.rs`

- `AccountManagers`: add `cursor: tokio::sync::Mutex<AccountManager>`.
- `build_account_managers`: construct the cursor manager with a refresh closure calling
  `cursor_auth::refresh_cursor_tokens`; `RefreshPolicy { kind: ExpiresLead, seconds: 600 }`
  (10-minute lead — ≥ the 10-min max stream so a token can't expire mid-stream, and far below the
  ~1h token lifetime so it refreshes once per token, never per-request). Load it, log its count.
- `UpstreamClient` trait: add `cursor_responses` and `cursor_responses_stream`. Real impl delegates
  to `src/cursor.rs` using the new `send_bytes`/`send_bytes_stream` helpers and
  `stream_messages_ms` for the timeout in **both** the stream and non-stream methods (Cursor always
  streams upstream; using `messages_ms` would truncate long non-stream reasoning responses). Test
  fakes get matching impls.
- `route_provider_request` dispatch: `ProviderId::Cursor => route_cursor_request(...)`.
- `route_cursor_request`: a near-clone of `route_codex_request` — translate the inbound body to a
  Responses request (reuse `codex_request_body` / a thin wrapper), stream vs non-stream,
  success/failure accounting.
- Group `Codex | Cursor` in: `json_upstream_response`, `transform_sse_event` (incl. the `[DONE]`
  arm), `update_stream_usage` (Cursor uses `update_codex_stream_usage`; usage stays zero),
  `count_tokens`'s unsupported-provider guard.
- `/v1/models`: append `cursor/composer-2.5` when the cursor manager has accounts (mirror the
  opencode-go block); add `ProviderId::Cursor => false` to the seed-list match.
- admin `/admin/accounts` and `/admin/reload`: include the cursor manager.
- the three account-manager lookup `match provider { ... }` sites: add `ProviderId::Cursor`.

### `src/cli.rs` + `src/runtime.rs`

- `cli.rs`: add `"cursor"` to the `--provider` `value_parser`; add `--cursor-import-local` bool flag
  on `Login`. Bail on meaningless combos (`--manual` with cursor, like the opencode-go guard).
- `CliRuntime::login`: extend with an `import_local: bool` parameter (consistent with `manual`/`key`).
- `runtime.rs` `login()`: when `provider == Cursor`, bypass the OAuth-code/callback path entirely —
  `--cursor-import-local` → `cursor_auth::import_cursor_local(...)` → `save_token`; otherwise print
  URL, open browser, `poll_cursor_auth(...)` → `TokenData` → `save_token`.

### `Cargo.toml`

- `rusqlite = { version = "0.32", features = ["bundled"] }` — vendored SQLite; self-contained
  release binary, no system `sqlite3`. (`unsafe_code = "forbid"` is crate-level; deps are unaffected.)
- `flate2 = "1"` — default `miniz_oxide` (pure Rust) backend, for per-frame gunzip.
- `uuid`: add the `"v5"` feature (already has `"v4"`).

## Data flow (chat request)

```
client POST /v1/{chat,messages,responses}
  → parse_request → resolve_model → registry.for_model   (cursor/ prefix → Cursor)
  → route_provider_request: pick a Cursor account (rotation/cooldown/refresh-if-due, ExpiresLead 600s)
  → route_cursor_request: inbound body → OpenAI-Responses request (reuse Codex translation)
  → cursor.rs: fold system into first user turn → encode protobuf → Connect frame
             → HTTP/2 POST api2.cursor.sh (send_bytes[_stream], stream_messages_ms timeout)
        stream:     decode frames → emit Responses SSE bytes (… response.completed)
        non-stream: accumulate frames → synthesize Responses JSON
  → existing Codex-path reshaping → client's requested format (chat / anthropic / responses)
  → record success/failure on the cursor AccountManager
```

## Error handling

- Reuse `AppError::provider(..., ProviderId::Cursor)`, the existing retry classifier
  (`should_retry_upstream_status`), and per-account backoff.
- **Classify Cursor upstream errors for backoff**: Connect/HTTP `unauthenticated`/401 → `auth`
  (→ 10–60 min cooldown), `resource_exhausted`/429 → `rate_limit` (→ 1–15 min), everything else →
  `server` (→ 5s–5 min). This drives the existing rotation/cooldown sensibly.
- HTTP/2 transport failure → `network_error` 502.
- Refresh exhaustion → `RefreshTokenExhaustedError` → 24h re-auth cooldown + a "re-run login"
  message (existing `record_refresh_exhausted`).
- Login: a poll result missing a refresh token, or a `state.vscdb` missing the auth keys, fails the
  `login` command with an actionable message; nothing is persisted.

## Testing

Unit (no network, pure functions):
- protobuf field encoders against known byte vectors; `encode_cursor_chat_request` golden bytes for a
  fixed message set.
- `build_cursor_checksum` / `jyh` encoder against reference vectors; machine-id fallback chain.
- system-fold: a body with `system` + `user` produces a single user turn whose content is
  `system\n\nuser` (and no system role is encoded).
- model normalization: `cursor/composer-2.5` → `composer-2.5`; `cursor/foo` → `foo`; empty →
  `composer-2.5`.
- `read_connect_frames` + `extract_from_payload`: hand-built frames (incl. a gzipped frame and a
  `</think>` split) decode to expected text/reasoning.
- `CursorStreamingDecoder` across arbitrary chunk boundaries.
- `cursor_token_from_storage`: a fixture storage map → expected `TokenData`.
- poll-result handling: missing refresh token → error; happy path → `TokenData`.

Integration (existing `app.rs` fake-`UpstreamClient` harness):
- `cursor/composer-2.5` routes to the Cursor manager; bare names do not.
- A fake `cursor_responses[_stream]` returning canned Responses SSE/JSON flows through to chat,
  anthropic, and responses client shapes correctly (proves Approach A reuse).
- `/v1/models` lists `cursor/composer-2.5` only when a cursor account is loaded.
- `count_tokens` for a `cursor/` model returns 501.
- tokens round-trip: a cursor token with meta saves/loads identically; an existing Anthropic token
  file is unaffected by the new optional fields.

## Implementation phasing (for the plan step)

1. **Scaffolding** (additive, compiles, all existing tests green): `ProviderId::Cursor`,
   `TokenData.cursor` meta, tokens storage round-trip, registry prefix routing, `/v1/models`,
   account-manager wiring (ExpiresLead 600s), a stub upstream returning a clear "not yet implemented".
2. **Auth**: `cursor_auth.rs` (browser poll + local import + refresh), CLI/runtime login wiring.
3. **Protocol**: `cursor.rs` (encode incl. system-fold, headers/checksum, `send_bytes` HTTP/2
   transport, frame decode, Responses SSE/JSON adaptation); replace the stub.

Each phase ends with a green build + tests.

## Risks / open questions

- **Wire-format fragility.** Field numbers, the checksum cipher, and the deep-control/poll URLs are
  reverse-engineered and can change without notice. Mitigation: keep client/config version swappable
  via config (`cloaking.cursor.*`) and token meta; isolate all magic constants in `cursor.rs`.
- **HTTP/2 via reqwest.** Assumes reqwest negotiates h2 to api2.cursor.sh and streams frames
  promptly. The transport sits behind `send_bytes`/`send_bytes_stream`, so swapping to the `h2`
  crate later is localized.
- **`bundled` rusqlite build cost.** Adds a C compile. Acceptable for a distributed binary; the
  shell-out-to-`sqlite3` option remains a documented fallback if it bloats CI.
- **No usage data.** Cursor stats show zero tokens. Documented; revisit if Cursor returns decodable
  usage.
- **Refresh lead is a guess (600s).** Tuned to an assumed ~1h access-token lifetime. If telemetry
  shows shorter tokens, shrink the lead; the policy shape does not change.
