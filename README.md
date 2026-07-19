# pengepul

A relay that pools your AI provider accounts behind one local endpoint. It speaks
the Anthropic Messages, OpenAI Chat Completions and OpenAI Responses APIs at once,
routes each request to a provider by model id, and translates between the three
shapes. Tokens rotate, refresh and back off on their own.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/gitshrl/pengepul/main/install.sh | sh
```

Linux x86_64 and macOS on Apple silicon. `PENGEPUL_BIN_DIR` changes the install
directory, `PENGEPUL_VERSION` pins a release.

Once installed, `pengepul update` replaces the binary in place with the latest
release, and `pengepul update --check` reports what is available without
installing it. Both verify the published checksum before replacing anything.

From source, against the Rust 1.96.0 pinned in `rust-toolchain.toml`:

```sh
cargo install --git https://github.com/gitshrl/pengepul.git --locked
```

## Login

`anthropic` and `codex` use OAuth; `opencode` uses a static API key.

```sh
pengepul login # defaults to anthropic
pengepul login --provider codex
pengepul login --provider opencode --key sk-... # or omit --key to import it
```

Without `--key`, opencode's key is read from its `auth.json` under
`$XDG_DATA_HOME/opencode`, accepting only an `opencode-go` entry of type `api`.
Prefer the import: `--key` is visible in process listings and shell history.

OAuth completes on a localhost callback, so logging in on a remote host needs the
port forwarded first:

```sh
ssh -L 54545:localhost:54545 user@host # anthropic
ssh -L 1455:localhost:1455 user@host # codex
```

Credentials live under `auth-dir` (`~/.pengepul`), one directory per provider,
`0600`. A running relay picks up a fresh login on restart or
`pengepul accounts --reload`.

## Run

```sh
pengepul # same as `pengepul serve`
pengepul serve --host 127.0.0.1 --port 8318
```

pengepul writes `~/.pengepul/config.yaml` when it is missing, and rewrites it
whenever `api-keys` is empty, generating a fresh `sk-local-…` key.

```yaml
host: ''
port: 8317
auth-dir: ~/.pengepul
api-keys:
  - sk-local-example
body-limit: 200mb
cloaking:
  cli-version: 2.1.88
  entrypoint: cli
  codex: {}
timeouts:
  messages-ms: 120000
  stream-messages-ms: 600000
  count-tokens-ms: 30000
stats:
  enabled: true
debug: off
```

An empty `host` binds `127.0.0.1`, not every interface. `debug` accepts `off`,
`errors` and `verbose`. Unknown keys are a hard load error.

## Commands

`serve` · `status` · `accounts` · `login` · `update` ·
`config path|show|api-key` ·
`service install|start|stop|restart|status|uninstall|logs` · `help`

`status` and `accounts` are HTTP calls to `/admin/*` on the running relay, so they
fail if it is down or its API key differs.

| Flag | Where | Effect |
|---|---|---|
| `--config <PATH>` | root, `serve`, `login`, `status`, `accounts`, `service install` | alternate config file |
| `--host` / `--port` | `serve`, `service install` | override the bind address |
| `--provider` | `login` | `anthropic` (default), `codex`, `opencode` |
| `--key <KEY>` | `login` | opencode API key, bypassing the import |
| `--start` / `--enable` | `service install` | start now / at login |
| `--lines <N>` / `--follow` | `service logs` | history (default 50) / stream |
| `--check` | `update` | report the available version without installing |

`--config` is not global. It must precede the subcommand unless that subcommand
declares its own, so `pengepul --config X config show` works and
`pengepul config show --config X` is rejected.

## Routes

`GET /health` (unauthenticated) · `GET /admin/accounts` · `POST /admin/reload` ·
`GET /v1/models` · `POST /v1/chat/completions` · `POST /v1/responses` ·
`POST /v1/messages` · `POST /v1/messages/count_tokens`

Every route but `/health` needs a key from `api-keys`, as either
`Authorization: Bearer <key>` or `x-api-key: <key>`.

The provider comes from the model id alone; the route only picks the translation
pair. An `opencode/` prefix wins first, then `gpt-5`, `gpt-5.*`, `gpt-5-*`, `o<N>`
and `codex-*` go to Codex, then `claude-*` to Anthropic. Anything else falls
through to Anthropic, so the `opencode/` prefix is mandatory — a bare `glm-5.1`
reaches Anthropic and is rejected there. `opus`, `sonnet` and `haiku` are aliases,
and a missing `model` becomes `claude-sonnet-4-6`.

Anthropic and Codex ids are pass-through, so `/v1/models` is a display list rather
than an accept list. opencode serves only `/v1/chat/completions`, and
`count_tokens` only Anthropic; other pairings return 501. `/v1/*` is rate limited
to 60 requests per minute per bucket, keyed on the first `x-forwarded-for` entry
then `x-real-ip`, with requests carrying neither sharing one bucket.

## Service

`pengepul service install` writes a systemd **user** unit on Linux or a launchd
agent on macOS, baking in any `--config`, `--host` and `--port` you pass.

```sh
pengepul service install --enable --start
pengepul service logs --follow
```

Because the unit is user-scoped, `systemctl status pengepul` and
`journalctl -u pengepul` will not find it — add `--user`, or use
`pengepul service status` and `pengepul service logs`. On macOS the plist always
sets `RunAtLoad` and `KeepAlive`, so `--enable` is ignored, and logs are files
under `~/.pengepul/logs/`.

## Examples

```sh
API_KEY=$(pengepul config api-key | tail -n1)

curl -sS http://127.0.0.1:8317/v1/messages \
  -H "Authorization: Bearer $API_KEY" -H "Content-Type: application/json" -d '{
  "model": "sonnet",
  "max_tokens": 128,
  "messages": [{"role": "user", "content": "reply exactly: pong"}]}'

curl -sS http://127.0.0.1:8317/v1/chat/completions \
  -H "Authorization: Bearer $API_KEY" -H "Content-Type: application/json" -d '{
  "model": "opencode/glm-5.1",
  "messages": [{"role": "user", "content": "reply exactly: pong"}]}'
```

Many opencode models spend tokens on hidden reasoning first, so leave `max_tokens`
unset or generous — too small a cap is consumed by reasoning and `content` comes
back empty with `finish_reason: length`.

## Clients

Point a client at `http://<host>:8317` with a key from `api-keys`. Two agent
frameworks that route their native Anthropic traffic through pengepul:

### openclaw

The embedded runner talks native Anthropic Messages. In `~/.openclaw/openclaw.json`,
define the `anthropic` provider against pengepul and select it with an `anthropic/`
prefixed model (a bare `claude-…` resolves to the claude-cli backend and bypasses
pengepul):

```json
{
  "agents": { "defaults": { "model": { "primary": "anthropic/claude-opus-4-8" } } },
  "models": {
    "providers": {
      "anthropic": {
        "baseUrl": "http://127.0.0.1:8317",
        "apiKey": "<pengepul api-key>",
        "auth": "api-key",
        "models": [
          { "id": "claude-opus-4-8", "api": "anthropic-messages", "contextWindow": 1000000, "maxTokens": 64000 }
        ]
      }
    }
  }
}
```

### hermes (Nous hermes-agent)

Register pengepul as a **named provider** on the native Messages wire — written via
`hermes config set` (into `HERMES_HOME/config.yaml`):

```sh
hermes config set model.provider pengepul
hermes config set model.default claude-opus-4-8
hermes config set providers.pengepul.base_url http://127.0.0.1:8317
hermes config set providers.pengepul.api_mode anthropic_messages
hermes config set providers.pengepul.api_key <pengepul api-key>
```

```yaml
model:
  provider: pengepul
  default: claude-opus-4-8
providers:
  pengepul:
    base_url: http://127.0.0.1:8317    # ROOT — the Anthropic SDK appends /v1/messages
    api_mode: anthropic_messages         # explicit → native Messages wire, no /anthropic suffix
    api_key: <pengepul api-key>          # sent as x-api-key verbatim; a plain sk-local-/pgpl- key is fine
```

- `api_mode: anthropic_messages` forces the native wire on a root `base_url`, so no
  extra pengepul route is needed.
- Use `provider: pengepul` (not `anthropic`) so hermes does **not** autodiscover the
  operator's `~/.claude` Claude-Code OAuth — that would force `api.anthropic.com` and
  bypass pengepul.
- Changing `providers.*.api_key` inside an existing `HERMES_HOME` caches the old
  key's rejection in `auth.json` ("No inference provider configured"); use a fresh
  home or delete `auth.json` when rotating.

## Behavior

- Account selection is strict round-robin, with no session affinity.
- A request fails over across accounts, retrying once per account on upstream 401,
  403, 429, 500 and 502-599, but never on 501.
- A failed account backs off 1s, 2s, 4s, 8s, … capped at 5 minutes, and resets on
  its next success. A dead refresh token locks it out for 24 hours instead, since
  only a fresh `pengepul login` clears it.
- Anthropic refreshes once expiry is under 4 hours away, Codex every 8 days,
  opencode never.
- A stream ending without its completion sentinel counts as a failure, even though
  the client already received a 200.
- `body-limit` is checked against `Content-Length` only, so a request without that
  header is rejected 411. An empty `body-limit` means unlimited.

## Logging

`serve` logs through `tracing` at `info`. Set `debug: verbose` for per-request
detail; `RUST_LOG` overrides both.
