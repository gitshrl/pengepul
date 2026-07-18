# pengepul

A single-binary relay that pools your AI provider accounts behind one local
endpoint. It speaks the Anthropic Messages, OpenAI Chat Completions and OpenAI
Responses APIs at once, routes each request to the right provider by model id,
and translates between the three shapes so any client can reach any account.
Tokens rotate, refresh and back off on their own.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/gitshrl/pengepul/main/install.sh | sh
```

Linux x86_64 and macOS on Apple silicon. No Rust, no build, no dependencies.

Set `PENGEPUL_BIN_DIR` to install somewhere other than `/usr/local/bin`, or
`PENGEPUL_VERSION` to pin a release instead of taking the most recent.

From source:

```sh
cargo install --git https://github.com/gitshrl/pengepul.git --locked
```

`rust-toolchain.toml` pins Rust 1.96.0, so a source build needs
`rustup toolchain install 1.96.0`.

## Login

Authorize at least one account before serving traffic. `anthropic` and `codex`
use OAuth; `opencode` uses a static API key.

```sh
pengepul login                            # same as --provider anthropic
pengepul login --provider codex
pengepul login --provider opencode --key sk-...
```

OAuth completes through a callback on localhost, so logging in on a remote host
needs that port forwarded before you start. The two providers differ:

```sh
ssh -L 54545:localhost:54545 user@host   # anthropic
ssh -L 1455:localhost:1455 user@host     # codex
```

Without `--key`, opencode's key is imported from its `auth.json` under
`$XDG_DATA_HOME/opencode` (falling back to `~/.local/share/opencode`), accepting
only an `opencode-go` entry of type `api`. Prefer the import: a key passed via
`--key` is visible in process listings and shell history.

Credentials live under `auth-dir` (`~/.pengepul` by default), one directory per
provider:

```
~/.pengepul/                          0700
  config.yaml                         0600
  anthropic/<email>.json              0600
  codex/<email>.json
  opencode/opencode-<hash>.json
```

A running relay does not notice a fresh login until you restart it or call
`pengepul accounts --reload`.

## Run

```sh
pengepul                              # same as `pengepul serve`
pengepul serve --host 127.0.0.1 --port 8318
```

pengepul writes `~/.pengepul/config.yaml` when it is missing, and rewrites it
whenever `api-keys` is empty, generating a fresh `sk-local-…` key. If
`~/.pengepul/config.yaml` is absent but a `config.yaml` sits in the working
directory, that file is read and migrated to the home path.

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
`errors` and `verbose`. Unknown keys are a hard load error, so a typo stops the
relay rather than being ignored.

## Commands

**Relay** `serve` (the default with no subcommand) · `status` · `accounts`

**Accounts** `login` · `accounts --reload`

**Config** `config path` · `config show` · `config api-key`

**Service** `service install` · `start` · `stop` · `restart` · `status` ·
`uninstall` · `logs`

`pengepul help <path>` prints help for any subcommand path, e.g.
`pengepul help service install`.

`status` and `accounts` are HTTP calls to `/admin/*` on the running relay, not
local file reads. They fail if the relay is down or the config API key differs
from the one the running instance loaded.

| Flag | Where | Effect |
|---|---|---|
| `--config <PATH>` | root, `serve`, `login`, `status`, `accounts`, `service install` | alternate config file |
| `--host` / `--port` | `serve`, `service install` | override the config's bind address |
| `--provider` | `login` | `anthropic` (default), `codex`, `opencode` |
| `--key <KEY>` | `login` | opencode API key, bypassing the `auth.json` import |
| `--start` / `--enable` | `service install` | start the unit now / at login |
| `--lines <N>` / `-n` | `service logs` | history to show, default 50 |
| `--follow` / `-f` | `service logs` | stream |

`--config` is not a global flag. It must precede the subcommand unless that
subcommand declares its own, so `pengepul --config X config show` works while
`pengepul config show --config X` is rejected.

## Routes

`GET /health` (unauthenticated) · `GET /admin/accounts` · `POST /admin/reload` ·
`GET /v1/models` · `POST /v1/chat/completions` (all three providers) ·
`POST /v1/responses` and `POST /v1/messages` (anthropic, codex) ·
`POST /v1/messages/count_tokens` (anthropic only)

Every route but `/health` needs an API key from `api-keys`, in either header
form:

```
Authorization: Bearer sk-local-example
x-api-key: sk-local-example
```

The provider is chosen by the model id alone, identically on all three POST
routes; the route only selects the translation pair. An `opencode/` prefix wins
first, then `gpt-5*` / `o<N>` / `codex-*` go to Codex, then `claude-*` to
Anthropic. Anything unrecognized falls through to Anthropic, so a bare
`glm-5.1` is sent to Anthropic and rejected upstream — the `opencode/` prefix is
mandatory. `opus`, `sonnet` and `haiku` are exact-match aliases, and a missing
`model` field becomes `claude-sonnet-4-6`.

Anthropic and Codex ids are pass-through, matched by prefix and forwarded
verbatim, so `/v1/models` is a display list rather than an accept list. Asking a
provider for a route it does not serve returns 501. `/v1/*` routes are rate
limited to 60 requests per minute per bucket, keyed on the first
`x-forwarded-for` entry, then `x-real-ip`; requests carrying neither header
share a single bucket. CORS is open to any origin.

## Service

`pengepul service install` writes a systemd **user** unit on Linux
(`~/.config/systemd/user/pengepul.service`) or a launchd agent on macOS
(`~/Library/LaunchAgents/dev.gitshrl.pengepul.plist`). `--start` starts it now;
`--enable` starts it at login. Any `--config`, `--host` and `--port` you pass to
`install` are baked into the unit, so the service and the CLI read the same
config.

```sh
pengepul service install --enable --start
pengepul service start | stop | restart | status | uninstall
pengepul service logs --follow
```

Because the unit is user-scoped, the system-scoped `systemctl status pengepul` and
`journalctl -u pengepul` will not find it. Use `pengepul service status` and
`pengepul service logs`, or add `--user` to the systemctl and journalctl calls.

On macOS the plist always sets `RunAtLoad` and `KeepAlive`, so an install always
starts at login and restarts on exit; `--enable` is ignored there. macOS logs are
files, not a journal: `service logs` tails `~/.pengepul/logs/service.log` and
`service.err.log`.

## Examples

```sh
API_KEY=$(pengepul config api-key | tail -n1)   # tail: the first run also prints where it saved the key

# anthropic, with web search
curl -sS http://127.0.0.1:8317/v1/messages \
  -H "Authorization: Bearer $API_KEY" -H "Content-Type: application/json" -d '{
  "model": "sonnet",
  "max_tokens": 128,
  "messages": [{"role": "user", "content": "Use web search and answer with only the current UTC date in ISO format."}],
  "tools": [{"type": "web_search_20250305", "name": "web_search"}]}'

# codex, on the Responses API
curl -sS http://127.0.0.1:8317/v1/responses \
  -H "Authorization: Bearer $API_KEY" -H "Content-Type: application/json" -d '{
  "model": "gpt-5.4",
  "input": [{"role": "user", "content": "reply exactly: pong"}],
  "max_output_tokens": 32}'

# opencode, chat completions only; -free ids hit the credits endpoint
curl -sS http://127.0.0.1:8317/v1/chat/completions \
  -H "Authorization: Bearer $API_KEY" -H "Content-Type: application/json" -d '{
  "model": "opencode/glm-5.1",
  "messages": [{"role": "user", "content": "reply exactly: pong"}]}'
```

Only the five `-free` ids pengepul knows route to the credits endpoint; any other
`-free` id goes to the paid plan. Many opencode models are reasoning models that
spend tokens on hidden reasoning first, so leave `max_tokens` unset or generous —
too small a cap is consumed by reasoning and `content` comes back empty with
`finish_reason: length`.

## Behavior

- Account selection is strict round-robin. Every request advances to the next
  account that is not cooling down; there is no session affinity.
- A single request fails over across accounts. It retries up to once per account
  on upstream 401, 403, 429, 500 and 502-599, but never on 501.
- A failed account backs off 1s, 2s, 4s, 8s, … per consecutive failure, capped at
  5 minutes, and resets on its next success or successful refresh.
- A dead OAuth refresh token locks that account out for 24 hours, since only a
  fresh `pengepul login` can clear it.
- Refresh cadences are fixed: Anthropic refreshes once expiry is under 4 hours
  away, Codex every 8 days, opencode never.
- A stream that ends without its completion sentinel counts as a failure and
  triggers backoff, even though the client already received a 200.
- Streaming responses are translated between Anthropic SSE, OpenAI chat chunks
  and Responses API events.
- `body-limit` is checked against the declared `Content-Length` only. A request
  without that header is rejected 411, which rules out chunked clients. An empty
  `body-limit` means unlimited.

## Logging

`serve` logs through `tracing` at `info`: the startup banner, per-provider
account counts, and upstream errors. Set `debug: verbose` in the config for
per-request URL and status detail. `RUST_LOG` overrides both.

```sh
RUST_LOG=pengepul=debug pengepul serve
```

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
```
