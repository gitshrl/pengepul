# openclaw-anthropic-filter

## Goal

openclaw runs as its own in-process agent (embedded runner, provider `anthropic`) and reaches Anthropic through pengepul; pengepul masquerades each request as a first-party Claude Code request so the Anthropic subscription billing classifier does not reject it (400 "Third-party apps now draw from your extra usage"). No claude CLI in the path.

## Non-goals

- No openclaw source changes (config only on the openclaw side).
- No change to pengepul's auth model, account pool, or non-Anthropic routes.
- No `count_tokens` masquerading (classifier fires on message send).
- No rewriting of message content, tool_result payloads, or thinking blocks — only tool *names* and the system prompt are transformed.
- No attempt to preserve openclaw's chat-behavior sections (Messaging/Heartbeats/Group Chats/Reply Tags) — they are stripped by design; coding/memory/skills instructions are kept.
- Production gateway untouched until an approved cutover.

## Settled design

- **Scope:** transform applies to every `POST /v1/messages` (+ stream) through pengepul.
- **Tool-name masquerade:** a deterministic, bijective map from each request's openclaw tool name → a Claude-Code-style pseudo-name (curated map for known openclaw tools, deterministic fallback for unknown). Applied to `tools[].name` and to tool references in the system prompt outbound; reversed on `tool_use.name` in responses (non-streaming and streaming SSE) so openclaw dispatches correctly. Deterministic-from-name so multi-turn history stays consistent.
- **System-prompt sanitization:** strip a hardcoded list of bot-section headings and their bodies (`## Messaging`, `### message tool`, `## Group Chats`, `### 💬 Know When to Speak!`, `### 😊 React Like a Human!`, `## Reply Tags`, `## Silent Replies`, `## Heartbeats`, `## 💓 Heartbeats - Be Proactive!`, `### Heartbeat vs Cron: When to Use Each`), replace the persona name `Lena` with a generic term, and rename tool refs to match the tool map. Log a warning if the system prompt no longer matches the expected shape (openclaw upgrade drift).

## Acceptance criteria

- AC-1: Given a request whose `tools[]` and system prompt use openclaw names, pengepul's transform produces `tools[]` and system-prompt tool refs using the mapped Claude-Code-style names; the map is deterministic (same openclaw name → same pseudo-name across calls) and bijective. Rust test.
- AC-2: A `tool_use` block in a response carrying a mapped name is reversed to the original openclaw name before reaching the client, in both non-streaming JSON and streaming SSE. Rust test.
- AC-3: The system-prompt sanitizer removes each listed bot section (heading + body up to the next heading), replaces `Lena`, and leaves kept sections (Tooling/Skills/Memory/SOUL/IDENTITY) present. Rust test.
- AC-4: `cargo test` and `cargo clippy --all-targets` pass clean.
- AC-5: Empirical — the captured real openclaw request body, sent through pengepul, returns `is_error:false` (no Third-party 400). Control: the same body with the transform disabled returns the 400.
- AC-6: End-to-end — a second gateway from the unpatched openclaw worktree, configured `anthropic/claude-opus-4-8` via pengepul (custom model def, baseUrl, api-key), completes a fresh session's first turn AND a turn that invokes at least one tool (proving the tool_use name reversal works round-trip), 3× fresh sessions.
- AC-7 (cutover, gated on explicit user go): production `openclaw.json` switched to the `anthropic/*` provider through pengepul, gateway ordered after pengepul, a forced-fresh production turn (incl. a tool call) succeeds.

## Verification

- `cargo test && cargo clippy --all-targets` in /home/me/code/pengepul (AC-1..4)
- Replay the captured body through pengepul: transform on → 200; transform off → 400 (AC-5)
- Test-gateway journal shows `is_error:false` for a fresh session and for a tool-invoking turn, 3× (AC-6)
- Post-cutover forced-fresh production turn with a tool call shows `is_error:false` (AC-7)
