# pengepul

## About

`pengepul` is a local OAuth-to-API relay for Claude and Codex accounts. It exposes familiar API routes while routing requests to the matching upstream provider, rotating local account tokens, refreshing credentials, and translating request/response shapes where needed.

The implementation is intentionally narrow:

- Claude models route to the Anthropic Messages API.
- GPT-5, o-series, and Codex models route to the Codex Responses backend.
- No other upstream providers are included.

## Install

```bash
python -m venv .venv
. .venv/bin/activate
pip install -e ".[dev]"
```

## Login

Authorize at least one account before serving traffic.

```bash
pengepul --login --provider anthropic
pengepul --login --provider codex
```

Use manual mode when the browser callback cannot reach localhost:

```bash
pengepul --login --provider anthropic --manual
pengepul --login --provider codex --manual
```

Tokens are stored under `~/.pengepul` by default.

## Run

```bash
pengepul --config config.yaml
```

If `config.yaml` does not exist, pengepul creates one with a generated API key.

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

## Behavior

- Account selection is round-robin with short sticky windows.
- Failed accounts enter provider-specific cooldowns.
- Tokens refresh before expiry or on the configured Codex refresh cadence.
- Streaming responses are translated between Anthropic SSE, OpenAI chat chunks, and Responses API events.
- Request body size is bounded by `body-limit`.
- CORS is limited to localhost origins.

## Verify

```bash
ruff check .
ruff format --check .
python -m compileall pengepul tests
python -m pytest -q
```
