# 4. Strip openclaw bot sections by heading keyword, and leave the persona name alone

Status: Accepted (amended by ADR-0005 — the keyword set is narrowed to the two
sections that actually trip; the categories named below are no longer stripped)

## Context

openclaw's embedded runner sends a system prompt built for a chat persona:
sections telling it when to speak in a group, how to react, how to use
heartbeats. Anthropic's billing classifier reads that prompt, and those sections
are what mark the request as a third-party bridge rather than a first-party CLI.
ADR-0002 covers the tool-name half of the same problem.

The first implementation matched a list of the exact headings openclaw emits —
`BOT_SECTION_HEADINGS`, holding `## Messaging`, `## 💓 Heartbeats - Be Proactive!`
and eight more. It also carried `PERSONA_NAME`/`PERSONA_REPLACEMENT` to swap the
operator's assistant name for a generic one. Both shipped, and both were removed
once the transform met a second openclaw version: an exact list breaks on any
release that rewords a heading, drops an emoji, or changes a heading's level, and
it breaks silently — the section survives, the classifier fires, and the only
symptom is the original 400 coming back.

## Decision

`is_bot_heading` lowercases a line and tests it against `BOT_SECTION_KEYWORDS`, a
set of short substrings. A matched heading of level L arms a skip that drops
everything up to the next heading of level ≤ L, so sub-sections leave with their
parent. The keyword set reaches categories the old heading list never had, among
them `reactions`, `authorized senders` and `inbound context`.

Nothing is protected. Sections such as Tooling, Skills and Memory survive by not
matching a keyword, not by appearing on an allow-list. There is no allow-list.

The persona name is deliberately not scrubbed. It is a value from the operator's
own workspace rather than an openclaw constant, and it does not move the
classifier. Scrubbing it bought nothing and coupled the transform to one
deployment's naming.

Tool renaming inside the prompt is confined to the `- <name>:` listing, so
surrounding prose is never rewritten. The transform runs from
`route_anthropic_request` on the Messages route only, which means an Anthropic
model on `POST /v1/messages` — a Codex- or Opencode-backed model on the same URL
is never masqueraded.

## Consequences

- A future openclaw can rename a section past the keyword net. Stripping stops,
  the classifier fires, and nothing logs a warning: `masquerade.rs` makes no
  `tracing` calls. The 400 is the only signal.
- Matching is against the whole lowercased line, `#` marks and path included, so
  a heading naming a file strips on the filename. A heading for `HEARTBEAT.md`
  goes, and because markdown comment lines starting with `#` parse as headings,
  a body line inside such a file can clear the skip and survive as an orphaned
  fragment.
- A section whose heading merely contains one of the keywords is stripped with no
  warning. The cost of a false positive is a lost instruction rather than a
  rejected request.
- The persona name reaches Anthropic in the outbound body. That is intended;
  `masquerade_leaves_persona_line_untouched` pins it so the behaviour cannot be
  reverted by accident.
- The unit tests prove the transform is self-consistent, never that the
  classifier still accepts its output. Re-validating that needs live traffic:
  replay `tests/fixtures/openclaw-embedded-body.json` through pengepul and expect
  success, then disable the transform in `route_anthropic_request` and expect the
  400. The fixture sets `stream: true`, so a replay answers with SSE rather than
  a JSON body. There is no config toggle; the control arm requires an edit. Run
  it after any openclaw upgrade.
