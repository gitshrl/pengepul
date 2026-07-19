# 7. Clients adapt to pengepul's routes; do not add per-client endpoints

Status: Accepted

## Context

pengepul exposes a fixed, generic route surface (`/v1/messages`,
`/v1/chat/completions`, `/v1/responses`, `/v1/models`, …), and the masquerade runs
as a transform on the Messages route (ADR-0002, ADR-0004, ADR-0005). Each new client
raises the question of how it reaches that surface.

hermes-agent speaks native Anthropic Messages, but selects the wire by a base-URL
convention borrowed from third-party Anthropic-compatible vendors: a `base_url`
ending in `/anthropic`, onto which its SDK appends `/v1/messages`. Pointed at
pengepul that yields `POST /anthropic/v1/messages` — a path pengepul does not serve.
An `/anthropic/v1/*` route alias was added to make it work, then reverted: it grows
the server's surface for one client's URL habit, and invites a route per future
client.

## Decision

pengepul's route surface stays generic. Clients adapt to it in their own config,
not the reverse:

- **openclaw** sets the anthropic provider's `baseUrl` to pengepul and selects an
  `anthropic/`-prefixed model, so its embedded runner posts native Messages to
  `/v1/messages`. The provider's config key must stay `anthropic`: openclaw derives
  a model's auth realm from its api family (`anthropic-messages` → realm
  `anthropic`), not from the config key, so the provider's inline `apiKey` resolves
  only when the key is `anthropic`. Renaming it — even with the key re-injected via
  `headers` — fails every turn with `No API key found for provider "anthropic"`.
- **hermes** registers a named provider with `api_mode: anthropic_messages` and a
  **root** `base_url` (`http://host:8317`). The explicit `api_mode` forces the
  native wire without the `/anthropic` suffix, so the SDK appends `/v1/messages`
  onto the root and lands on the existing route. Naming the provider anything but
  `anthropic` also stops hermes autodiscovering the operator's `~/.claude`
  Claude-Code OAuth, which would otherwise force `api.anthropic.com` and bypass
  pengepul entirely.

Both shapes are in the README's Clients section.

## Consequences

- The route table does not grow per client, and the masquerade stays a single
  Messages-route transform.
- The cost moves to the operator: each client needs its specific config, and a
  client that hard-codes a path pengepul does not serve (the `/anthropic` suffix,
  a vendor prefix) will not work until its config is corrected — there is no
  server-side accommodation.
- A future client that cannot be configured to hit `/v1/*` on a root base URL would
  reopen this decision. The bar for adding a route is that a real client genuinely
  cannot adapt, not that adapting is inconvenient.
- The two clients are forced to opposite provider names for opposite reasons:
  openclaw's config key must be `anthropic` (auth realm follows the api family),
  hermes's must not be (a non-`anthropic` name is what stops `~/.claude` OAuth
  autodiscovery). The naming difference between the two configs is a constraint,
  not an inconsistency to reconcile.
