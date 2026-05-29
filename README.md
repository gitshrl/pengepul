# pengepul

## About

`pengepul` is a relay for Claude and Codex accounts. It exposes familiar API routes while routing requests to the matching upstream provider, rotating account tokens, refreshing credentials, and translating request/response shapes where needed.

The implementation is intentionally narrow:

- Claude models route to the Anthropic Messages API.
- GPT models route to the Codex Responses backend.
- `opencode-go/<model>` routes to OpenCode Go (OpenAI chat-completions) on `/v1/chat/completions`.

## Install

Download a prebuilt binary from the latest GitHub Release:

```bash
curl -L https://github.com/gitshrl/pengepul/releases/latest/download/pengepul-x86_64-unknown-linux-gnu.tar.gz | tar -xz
```

Release archives are published for:

- `x86_64-unknown-linux-gnu`
- `aarch64-apple-darwin`

Or install the command from source with Cargo:

```bash
cargo install --git https://github.com/gitshrl/pengepul.git --locked
```

For local development from this checkout:

```bash
cargo install --path . --locked --force
```

This repository is pinned to Rust 1.95.0 through `rust-toolchain.toml`.

```bash
rustup toolchain install 1.95.0
```

## Login

Authorize at least one account before serving traffic.

```bash
pengepul login --provider anthropic
pengepul login --provider codex
```

Use manual mode when the browser callback cannot reach localhost:

```bash
pengepul login --provider anthropic --manual
pengepul login --provider codex --manual
```

opencode-go uses a static API key instead of OAuth. With opencode installed and signed in,
pengepul imports the key from its `auth.json`; otherwise pass it explicitly:

```bash
pengepul login --provider opencode-go
pengepul login --provider opencode-go --key sk-...
```

Prefer the import path: a key passed via `--key` is visible in process listings (`ps`) and
shell history, whereas the import reads it directly from opencode's stored credentials.

Route opencode-go models with the `opencode-go/` prefix on `/v1/chat/completions`, e.g.
`opencode-go/glm-5.1`. The available ids are listed by `GET /v1/models` once a key is loaded.

Tokens are stored under `~/.pengepul` by default.

## Run

```bash
pengepul
```

```bash
pengepul serve
```

Bind a custom host or port:

```bash
pengepul serve --host 127.0.0.1 --port 8318
```

If `~/.pengepul/config.yaml` does not exist, pengepul creates one with a generated API key.

```yaml
host: ""
port: 8317
auth-dir: ~/.pengepul
api-keys:
  - sk-local-example
body-limit: 200mb
timeouts:
  messages-ms: 120000
  stream-messages-ms: 600000
  count-tokens-ms: 30000
stats:
  enabled: true
debug: off
```

Use `--config /path/to/config.yaml` only when you intentionally want a custom config path.

## Commands

```bash
pengepul
pengepul serve
pengepul serve --host 127.0.0.1 --port 8318
pengepul login --provider anthropic
pengepul login --provider codex
pengepul login --provider opencode-go
pengepul login --provider opencode-go --key sk-...
pengepul login --provider anthropic --manual
pengepul login --provider codex --manual
pengepul help
pengepul help service install
pengepul status
pengepul accounts
pengepul accounts --reload
pengepul config path
pengepul config show
pengepul config api-key
pengepul service install --start
pengepul service install --host 127.0.0.1 --port 8318 --start
pengepul service install --enable --start
pengepul service start
pengepul service status
pengepul service restart
pengepul service stop
pengepul service uninstall
```

Service install supports Linux systemd and macOS launchd. On Linux, use `--enable` to start the user service at login.

## Development

Run the Rust quality gates:

```bash
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
```

## Release

Releases are built from `v*` tags by GitHub Actions. The tag should match the Cargo package
version:

```bash
git tag v0.1.0
git push origin v0.1.0
```

## Logging

`serve` logs via `tracing`. The level follows the `debug` config key (`off`/`errors` log the
startup banner and upstream errors at `info`; `verbose` adds per-request detail at `debug`).
`RUST_LOG` overrides it, e.g.:

```bash
RUST_LOG=pengepul=debug pengepul serve
```

## Routes

- `GET /health`
- `GET /admin/accounts`
- `POST /admin/reload`
- `GET /v1/models`
- `POST /v1/chat/completions`
- `POST /v1/responses`
- `POST /v1/messages`
- `POST /v1/messages/count_tokens`

Use either header form:

```bash
Authorization: Bearer sk-local-example
x-api-key: sk-local-example
```

## Examples

Load the generated API key from the default config:

```bash
API_KEY=$(awk '/api-keys:/{getline; sub(/^[[:space:]]*-[[:space:]]*/, ""); print; exit}' ~/.pengepul/config.yaml)
```

Anthropic / Claude web search:

```bash
curl -sS http://127.0.0.1:8317/v1/messages \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "sonnet",
    "max_tokens": 128,
    "messages": [
      {
        "role": "user",
        "content": "Use web search and answer with only the current UTC date in ISO format."
      }
    ],
    "tools": [
      {
        "type": "web_search_20250305",
        "name": "web_search"
      }
    ]
  }'
```

Codex login, then restart `pengepul` before testing Codex routes:

```bash
pengepul login --provider codex
```

Confirm Codex account is loaded:

```bash
curl -sS http://127.0.0.1:8317/admin/accounts \
  -H "Authorization: Bearer $API_KEY"
```

Codex basic Responses request:

```bash
curl -sS http://127.0.0.1:8317/v1/responses \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-5.5",
    "input": [
      {
        "role": "user",
        "content": "reply exactly: pong"
      }
    ],
    "max_output_tokens": 32
  }'
```

Codex web search:

```bash
curl -sS http://127.0.0.1:8317/v1/responses \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-5.5",
    "input": [
      {
        "role": "user",
        "content": "Use web search and answer with only the current UTC date in ISO format."
      }
    ],
    "max_output_tokens": 128,
    "reasoning": {
      "effort": "xhigh"
    },
    "tools": [
      {
        "type": "web_search"
      }
    ]
  }'
```

## Behavior

- Account selection is round-robin with short sticky windows.
- Failed accounts enter provider-specific cooldowns.
- Tokens refresh before expiry or on the configured Codex refresh cadence.
- Streaming responses are translated between Anthropic SSE, OpenAI chat chunks, and Responses API events.
- Request body size is bounded by `body-limit`.
