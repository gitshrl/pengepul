# 9. Match one generated heading exactly, where no keyword can separate it

Status: Accepted

## Context

ADR-0004 strips openclaw's classifier-tripping system sections by heading
keyword, and ADR-0005 narrowed that keyword set to the two sections that actually
400 on openclaw 2026.7.x. That set was bisected against one openclaw line. A
second, older deployment runs 2026.3.31, which names its equivalents differently:
nothing in the keyword set matches, nothing is stripped, and every request fails
with the billing 400. Tools appear broken because the whole turn is rejected, not
because tool renaming is wrong — it round-trips correctly on both lines, on the
JSON and SSE egress paths.

Bisected live against the classifier with a greedy shrink (start from every
generated section removed, restore one at a time, treat a 503 as backoff rather
than a verdict), 2026.3.31 trips on exactly two generated sections:
`## Reply Tags`, carrying the `[[reply_to_current]]` tag protocol, and
`## Heartbeats`, carrying the `HEARTBEAT_OK` ack protocol. The other fourteen
generated sections pass and are kept.

`## Reply Tags` collides with nothing and takes a keyword. `## Heartbeats` does
not: it is a strict prefix of `## Heartbeats (if configured)`, an operator section
in the injected `AGENTS.md` that passes the classifier and has to survive on both
lines. No substring separates a prefix from its extension.

Matching the section body instead — the literal `[[reply_to_current]]` or
`HEARTBEAT_OK` — was designed and rejected. An injected `HEARTBEAT.md` writes its
own comments as `#` lines, which parse as level-1 headings; arming a skip on one
runs to the end of the block and silently swallows every section after it. The
operator putting `HEARTBEAT_OK` in the file whose purpose is heartbeat text would
amputate the rest of the prompt.

## Decision

`BOT_SECTION_KEYWORDS` gains `reply tag`, keeping the substring mechanism and its
tolerance for reworded variants.

`BOT_SECTION_HEADINGS` is a whole-line exact set holding one entry,
`## heartbeats`, compared against the lowercased line with trailing space trimmed.
`is_bot_heading` matches either. Entries carry their own `## `, which is what
bounds the blast radius: no level-1 line can ever arm a skip, so the failure that
sank the body-literal design is structurally impossible here.

Only `heartbeats` earns exact-match status, on the prefix argument above. Nothing
else joins it for symmetry.

## Consequences

- ADR-0004 rejected exact heading lists because they break silently when a release
  rewords a heading. That objection is answered rather than dismissed: this entry
  targets a frozen artifact. 2026.3.31 is released and its prompt text does not
  change, so the entry cannot rot out from under a running deployment.
- The two failure directions are not symmetric, and the rule is deliberately
  biased. Under-stripping fails loud: a hard 400 on every request, noticed
  immediately. Over-stripping fails silent: operator instructions vanish with no
  signal, which is the regression ADR-0005 was written to undo. Exact matching
  takes the loud side.
- A future openclaw that ships `## Heartbeats — Polling` or similar is not matched,
  and the 400 returns for that release until an entry is added. That is the
  intended direction, not an oversight.
- `## Heartbeats - Be Proactive!` stays kept, as ADR-0005 established; the existing
  `system_prompt_strips_only_the_two_classifier_sections` test still pins it.
- Three tests pin the new behaviour, each mutation-proven to fail when the
  production code is broken: the generated-versus-operator Heartbeats split, the
  `HEARTBEAT.md` level-1 comment lines never arming a skip, and Reply Tags being
  keyword-tier. The level-1 test needs a `# Heartbeats` line in its fixture to bite;
  without one it passes even with the anchor removed.
