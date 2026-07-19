# 8. Swap openclaw's web tools for Anthropic server tools, and strip orphaned thinking

Status: Accepted

## Context

openclaw ships its own client-executed `web_search` and `web_fetch` tools — it runs
the search itself and feeds the result back as a `tool_result`. On the
subscription, the aim is to let Anthropic run the search server-side instead
(`web_search_20250305`, `web_fetch_20250910`), so results and citations fold into
the turn without a client round-trip and the call reads as first-party.

Native server tools do not come back as a plain `tool_use`. They return `thinking`,
then `server_tool_use`, then `*_tool_result`, then text. openclaw renders the text
and persists the `thinking`, but drops the `server_tool_use`/`*_tool_result` blocks
it does not model. On the next turn it replays that assistant message with
`thinking` blocks that no longer sit against the `server_tool_use` they were signed
over, and Anthropic rejects it: `thinking blocks in the latest assistant message
cannot be modified` — a deterministic, non-retryable 400. This was the "web search
from telegram keeps failing" report, invisible to a first, single search but fatal
to the turn after it.

## Decision

`native_replacement` swaps `web_search` for `{"type": "web_search_20250305",
"name": "web_search", "max_uses": 5}` and `web_fetch` for the `web_fetch_20250910`
equivalent. Both keep their real names and are excluded from the PascalCase map, so
they are never client-dispatched or reverse-mapped. `web_fetch` additionally needs
`web-fetch-2025-09-10` in the beta header (`build_beta_header`).

To keep multi-turn history valid, `strip_orphan_thinking` drops `thinking` and
`redacted_thinking` from any assistant turn that is **not** immediately answered by
a `tool_result`. A live tool-use continuation — assistant `tool_use` followed by a
user `tool_result` — keeps its thinking; every completed turn, including the mangled
web-search turn, loses it. Anthropic requires thinking to be preserved only on the
active continuation, so dropping it elsewhere is safe.

## Consequences

- Search and fetch run server-side; the client no longer executes them. openclaw
  tolerates the server blocks (renders text + `<cite>` citations) and does not
  persist them — which is exactly why the orphan-thinking strip is required, not
  optional.
- Semantics shift: Anthropic's native `web_fetch` may only fetch a URL already in
  context (a search result or a user-supplied link), unlike openclaw's own fetch.
  A search-then-fetch flow is unaffected; an arbitrary-URL fetch is not.
- Stripping thinking from completed turns discards that reasoning in later turns —
  cheap, since thinking is ephemeral, and it saves tokens.
- Only these two exact names are swapped. A client whose web tool is named
  otherwise is untouched and stays a normal PascalCased client tool.
- The swap alone does not make web search first-party; it rides on the masquerade
  and beta headers already in place. Remove those and the server call is rejected.
- Tests: `tool_names_are_pascalcased...` pins the native `type` on both tools;
  `strips_thinking_from_completed_turns_but_keeps_tool_continuation` pins the strip
  rule (kept on a tool-result-answered turn, dropped on a completed one).
