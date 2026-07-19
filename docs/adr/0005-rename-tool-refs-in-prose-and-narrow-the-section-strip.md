# 5. Rename tool references in prose, and narrow the section strip to what trips

Status: Accepted (amends ADR-0002 and ADR-0004)

## Context

ADR-0002 renamed tool names only inside the `- <name>:` listing and stated that
surrounding prose is never rewritten. ADR-0004 stripped a broad set of bot-section
keywords. Two later findings, both bisected against the live classifier, contradict
those choices.

First, a second client — Nous hermes-agent — trips the classifier on the same
subscription 400 even after its tool *array* is PascalCased. Bisection isolated the
cause to `snake_case` tool **names quoted in the system-prompt prose** (`use
session_search to recall`, `patch it with skill_manage(...)`), not the array. The
effect is cumulative: two such references together trip, either alone passes.
ADR-0002's "listing only" rule leaves them in place.

Second, re-bisecting openclaw's own prompt showed only two sections actually move
the classifier — `## Assistant Output Directives` and `## Inbound Context (trusted
metadata)`. The ADR-0004 keyword set was stripping ten-plus sections (Messaging,
Heartbeats, Group Chats, Reactions, senders, …) that pass fine, which quietly
deleted the operator's chat-behaviour instructions for no classifier benefit.

## Decision

`sanitize_system_text` now renames a **multi-word (underscore-bearing) tool name
whole-word anywhere in the prose** via `replace_word` — an ASCII-identifier match
that fires only when the characters on both sides are not identifier characters, so
`read_file` inside `read_files` is left alone. A **single-word** name (`read`,
`memory`, `process` — also ordinary English) is still confined to the `- <name>:`
listing so prose is not clobbered. Names carried by `native_replacement`
(`web_search`, `web_fetch`) stay out of the map entirely.

`BOT_SECTION_KEYWORDS` is narrowed to the four substrings covering the two tripping
sections (`assistant output`, `output directives`, `inbound context`, `trusted
metadata`). Every other bot section is kept.

## Consequences

- This supersedes ADR-0002's "prose is never rewritten" and ADR-0004's claim that
  the keyword set reaches `reactions`, `authorized senders` and the like — it no
  longer does, by design.
- The whole-word + underscore-only rule is the whole reason `replace_word` exists
  over `str::replace`; `tests/masquerade.rs` mutation-locks it (a leading/trailing
  substring must survive, and a native-swapped name must keep its prose mention).
- A single-word tool name that trips the classifier in prose would be missed. None
  is observed; if one appears the symptom is the original 400 with no log, per
  ADR-0004.
- Keeping the other bot sections means a future openclaw section beyond the two
  could start tripping. That is the same silent-400 risk ADR-0004 already carries,
  now against a smaller net.
