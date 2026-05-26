# pengepul

## About

`pengepul` is an OAuth-to-API relay for Claude and Codex accounts. It exposes familiar API routes while routing requests to the matching upstream provider, rotating account tokens, refreshing credentials, and translating request/response shapes where needed.

The implementation is intentionally narrow:

- Claude models route to the Anthropic Messages API.
- GPT-5, o-series, and Codex models route to the Codex Responses backend.
- No other upstream providers are included.

## Install

```bash
uv sync --extra dev
```

## Login

Authorize at least one account before serving traffic.

```bash
.venv/bin/pengepul --login --provider anthropic
.venv/bin/pengepul --login --provider codex
```

Use manual mode when the browser callback cannot reach localhost:

```bash
.venv/bin/pengepul --login --provider anthropic --manual
.venv/bin/pengepul --login --provider codex --manual
```

Tokens are stored under `~/.pengepul` by default.

## Run

```bash
.venv/bin/pengepul
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
.venv/bin/pengepul --login --provider codex
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
    "model": "gpt-5.4",
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
    "model": "gpt-5.4",
    "input": [
      {
        "role": "user",
        "content": "Use web search and answer with only the current UTC date in ISO format."
      }
    ],
    "max_output_tokens": 128,
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
- CORS is limited to localhost origins.

## Verify

```bash
.venv/bin/ruff check .
.venv/bin/ruff format --check .
.venv/bin/python -m compileall pengepul tests
.venv/bin/python -m pytest -q
```
