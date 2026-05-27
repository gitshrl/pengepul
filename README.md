# pengepul

## About

`pengepul` is a relay for Claude and Codex accounts. It exposes familiar API routes while routing requests to the matching upstream provider, rotating account tokens, refreshing credentials, and translating request/response shapes where needed.

The implementation is intentionally narrow:

- Claude models route to the Anthropic Messages API.
- GPT, o-series, and Codex models route to the Codex Responses backend.
- Other upstream providers are not included yet.

## Install

Install the command with uv:

```bash
uv tool install git+https://github.com/gitshrl/pengepul.git
```

For local development from this checkout:

```bash
uv tool install --editable . --force
```

Install development dependencies only when working on the repo:

```bash
uv sync --extra dev
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

## CLI

```bash
pengepul help
pengepul help service install
pengepul status
pengepul accounts
pengepul accounts --reload
pengepul config path
pengepul config show
pengepul config api-key
```

Install git hooks for local quality gates:

```bash
uv sync --extra dev
.venv/bin/pre-commit install --hook-type pre-commit --hook-type pre-push
```

## Service

Install a user service on Linux systemd or macOS launchd:

```bash
pengepul service install --start
```

Persist a custom bind address in the service:

```bash
pengepul service install --host 127.0.0.1 --port 8318 --start
```

On Linux, enable the user service at login:

```bash
pengepul service install --enable --start
```

Manage the service:

```bash
pengepul service status
pengepul service restart
pengepul service stop
pengepul service uninstall
```

## Docker

Build and run:

```bash
docker build -t pengepul .
docker run --rm -p 8317:8317 \
  -v "$HOME/.pengepul:/home/pengepul/.pengepul" \
  pengepul
```

Or use Compose:

```bash
docker compose up --build
```

The simplest Docker login flow is to login on the host first, then mount `~/.pengepul` into the container.

```bash
pengepul login --provider anthropic
pengepul login --provider codex
```

If you need to login inside the container, use manual mode:

```bash
docker run --rm -it \
  -v "$HOME/.pengepul:/home/pengepul/.pengepul" \
  pengepul login --provider anthropic --manual
```

Open the printed URL in the host browser. If the browser cannot reach the callback URL, copy the final browser URL containing `code` and `state`, then paste it back into the container prompt.

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
        "type": "web_search_20260209",
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
