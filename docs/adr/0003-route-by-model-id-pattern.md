# 3. Route by model-id pattern, with an explicit prefix only for opencode

Status: Accepted

## Context

A client reaching pengepul on `/v1/chat/completions`, `/v1/messages`,
`/v1/messages/count_tokens` or `/v1/responses` names a model and nothing else.
Nothing in the request says which upstream should answer it, and pengepul serves
three: anthropic, codex and opencode.

Two of the three own their namespace outright. Anthropic's ids all begin
`claude-`; codex's are `gpt-5*`, `o<digit>` and `codex-*`. Opencode owns
nothing — it resells other vendors' catalogs under those vendors' own ids,
`glm-5.1`, `kimi-k2.6`, `deepseek-v4-flash` (`OPENCODE_MODELS` in
`src/providers.rs`). No pattern picks opencode out, and a bare `glm-5.1` is
exactly the id a user of some other tool would send.

Requiring `<provider-id>/<model>` on every request removes the guessing
entirely. It also breaks every deployed client configuration across all four
routes at once, to disambiguate namespaces that do not collide.

## Decision

Routing is `ProviderRegistry::for_model` (`src/providers.rs:51`), reached from
`route_provider_request` (`src/app.rs:677`) and `count_tokens`
(`src/app.rs:603`). It resolves aliases, tries the literal `opencode/` prefix,
then `^(gpt-5(\.|-)|gpt-5$|o\d|codex-)` for codex, then `^claude-` for
anthropic, and returns anthropic when nothing matched. Opencode alone carries a
prefix, and `strip_opencode_prefix` removes it before the body goes upstream.
`/v1/models` advertises the ids that route: opencode's namespaced
(`src/app.rs:541`), anthropic's and codex's bare (`src/app.rs:517-519`).

## Consequences

- An id no pattern recognises reaches an Anthropic endpoint that has never heard
  of it, and the failure surfaces as someone else's 404 rather than as a routing
  error. The test at `src/providers.rs:166` pins that behavior: bare `glm-5.1`
  routes to anthropic, deliberately, so that an opencode id cannot hijack a
  provider by accident.
- Anthropic is the default provider by construction. A request omitting `model`
  gets `claude-sonnet-4-6` from `resolve_model(None)` and lands on anthropic
  without any pattern being consulted.
- The prefix rule is asymmetric and stays that way. `opencode/glm-5.1` routes,
  `glm-5.1` does not, which reads as a bug to anyone meeting it cold and is the
  price of leaving anthropic and codex clients untouched.
- A fourth provider serving `gpt-*` cannot be added under this rule. Every id a
  provider claims has to be read against three patterns compiled into the
  binary, and the first overlap mis-routes silently, which is the failure mode
  an unconditional fallback produces.
- Both regexes are rebuilt inside `for_model` on every request. The cost is per
  routing decision, not per process.
