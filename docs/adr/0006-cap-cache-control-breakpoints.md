# 6. Cap cache_control breakpoints, dropping the injected prefix first

Status: Accepted

## Context

`apply_cloaking` prepends two system blocks — the billing header and a `You are
Claude Code, Anthropic's official CLI for Claude.` prefix — and the prefix carries
`cache_control: ephemeral`, matching what real Claude Code sends.

Anthropic caps a request at four `cache_control` breakpoints, and counts them
across `system`, `tools`, and every message's `content` together. A client that
already spends its full budget of four leaves no room for the prefix: the total
becomes five and Anthropic rejects it with a non-retryable `A maximum of 4 blocks
with cache_control may be provided. Found 5.`

hermes-agent is such a client. It marks four breakpoints, and from the second turn
onward one of them lands on the assistant message it just cached. A single-shot
turn carries fewer breakpoints and passes, so the fault surfaced only in a running
conversation — the gateway was hard-down while `-z` one-shots looked healthy.

## Decision

`cap_cache_control` runs at the end of `apply_cloaking`. It counts `cache_control`
blocks across `system` + `tools` + `messages[].content`, and if the total exceeds
four it removes `cache_control` from the **earliest** marked blocks until four
remain. Our prefix is the first marked block in `system`, so it is the first to
lose its marker; the client's later breakpoints — which cache the longer, more
valuable prefixes — are kept.

Rejected: never marking the prefix at all. It is simpler and fixes the +1 case, but
it does nothing for a client that already sends more than four on its own, and it
still leaves the general "merged request exceeds the provider limit" shape
unhandled. The cap subsumes both.

## Consequences

- Dropping a breakpoint is always safe for correctness. `cache_control` is an
  optimisation hint; removing it never changes the response, only cache efficiency.
- Our prefix loses its own cache segment when the client is at budget. That segment
  is a ten-token static line whose content is still cached under the client's next
  breakpoint, so the loss is negligible.
- Stripping the earliest rather than the latest is deliberate: Anthropic caches the
  prefix ending at each breakpoint, so the later breakpoints cover more content.
  Keeping them and dropping the earliest preserves the most caching.
- `apply_cloaking_caps_cache_control_at_four_across_system_tools_messages` pins the
  cross-location count and that the prefix, not a client block, is the one dropped.
