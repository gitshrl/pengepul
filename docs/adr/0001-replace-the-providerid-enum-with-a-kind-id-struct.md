# 1. Replace the ProviderId enum with a {kind, id} struct

Status: Accepted

## Context

Provider identity does two unrelated jobs. It selects behavior — which account
manager holds the credentials, which OAuth flow mints the token, which
translator reshapes the body, which parser reads usage off the stream — and it
names a provider on the outside, as the directory under the auth dir holding
that provider's tokens and as the string `/v1/models` reports in `owned_by`.

For anthropic, codex and opencode those two jobs collapse onto one string, so a
closed `enum ProviderId { Anthropic, Codex, Opencode }` carries both. It stops
carrying both the moment behavior and naming stop being one-to-one.
Taking pengepul to roughly thirty upstreams means fifteen or so
OpenAI-compatible API-key services — groq, mistral, deepseek, openrouter and the
rest — sharing one header builder, one translator and one auth model while
needing fifteen distinct credential directories. An enum variant is a line of
Rust. groq is a base URL and a key.

## Decision

`ProviderId` at `src/types.rs:45-49` pairs a closed `ProviderKind` enum with an
`Arc<str>` id. `kind` drives behavior and nothing else: the account manager pick
in `src/app.rs:1123`, the auth URL and callback endpoint in `src/runtime.rs:238`
and `:246`, the native format in `src/providers.rs:29`, the on-disk `token_type`
discriminator frozen at the v0.1 names in `src/tokens.rs:187`. `id` drives
naming and nothing else: `storage_dir()` returns it, `tokens::save_token` joins
it onto `auth_dir`, and `Display` prints it. No `id` value reaches control flow.
Behavior stays closed and exhaustively matched. Identity stays open.

There is a third shape. Keeping the enum and giving one variant a payload,
`GenericOpenAi(Arc<str>)`, pays the same loss of `Copy` the struct pays, but
leaves the id reachable only through a method that fabricates a string for every
other variant. The struct spends that cost once and gets a field every provider
can be asked for.

## Consequences

- `ProviderId` derives `Clone` and not `Copy`. Its 93 references across eight
  files in `src/` settle clone-versus-borrow per call site rather than once:
  `ProviderRegistry::get` hands back a cloned `Provider`, and
  `next_provider_account` takes the id by value. `Arc<str>` keeps a clone down
  to a refcount bump, asserted by `provider_id_clone_shares_arc`.
- Every kind in the registry has exactly one id, equal to `kind.canonical_id()`.
  `ProviderId::new` has no caller outside `src/types.rs`, and `FromStr` derives
  the id from the parsed kind, so the field reads as a heap copy of the enum's
  own name, and reading `src/types.rs` cold gives no hint why it exists.
- The layout on disk binds to `id`; the discriminator inside the file binds to
  `kind`. `save_token` writes `<auth_dir>/<id>/<email>.json`, `load_all_tokens`
  picks the directory by `id` and then keeps a file only when the kind it
  rebuilds matches, and `storage_to_token` rebuilds that kind from `token_type`
  while ignoring the directory it came from. Two instances of one kind are
  indistinguishable on the read path.
- The type admits a second instance of a kind. The constructor does not build
  one: `build_registry` ignores its `auth_dir` argument and returns three
  hardcoded providers, and `src/config.rs` has no provider concept, so an
  instance declared outside Rust has nothing that constructs it.
- Adding a kind stops the compiler at every behavior site. `ProviderKind` is
  closed, so the exhaustive `match` arms in `app.rs`, `runtime.rs`,
  `providers.rs` and `tokens.rs` are the checklist for the work. That property
  is the reason not to reach for a trait object or a bare string tag.
