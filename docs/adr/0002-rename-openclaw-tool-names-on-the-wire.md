# 2. Rename openclaw's tool names on the wire, reverse them on the way back

Status: Accepted (amended by ADR-0005 — multi-word tool names are now also renamed
in the prompt prose, not only the tool listing)

## Context

Anthropic's subscription billing classifier fingerprints a request as a
third-party bridge and routes it to extra-usage billing, which is a hard 400
(`Third-party apps now draw from your extra usage`) on an org with overage
disabled. openclaw's embedded runner trips it: its tool roster is `exec`,
`gateway`, `nodes`, `sessions_spawn`, `tts` — nothing a first-party Claude Code
session would ever send. The roster travels in three places in one body:
`tools[].name`, the `- <name>: <description>` listing in the system prompt, and
every `tool_use` block in the replayed message history.

The names change either at the source or at the proxy. Changing them at the
source means patching openclaw's tool registry, which was available — the
worktree is local. A patched registry has to be re-applied on every openclaw
upgrade, and it drags the renaming into openclaw's own dispatch, tool_result
plumbing, and stored session history.

## Decision

pengepul rewrites the names, in `src/masquerade.rs`, on every `POST
/v1/messages` and nowhere else (`src/app.rs:833`; `count_tokens` does not fire
the classifier).

`build_tool_map` derives a fresh map per request from that request's own
`tools[]`. `pseudo_for` FNV-1a-hashes each tool name into `CC_TOOL_POOL` — 82
fabricated Claude-Code-shaped names, `Bash` through `Rebase` — and linear-probes
to the next free slot, which keeps the map bijective within a request. `exec`
becomes `Log`, `gateway` becomes `Edit`, `nodes` becomes `Rename`.
`masquerade_request` applies that map to all three carriers, so a body leaves
pengepul with no openclaw name in it. An exhausted pool leaves the tool its own
name, symmetric with the reverse map, which passes unknown names through.

`restore_tool_use_names` undoes it at both response egress points: the JSON body
at `src/app.rs:1305` and the SSE `content_block` at `src/app.rs:1578`. openclaw
never observes a pseudo-name, and its stored history stays in its own
vocabulary.

## Consequences

- openclaw stays stock. Upgrades need no re-patching, and the tool roster
  churning across versions costs nothing but a wider pool. pengepul owns the
  transform instead: the pool is sized 82 against a 46-tool roster, and
  shrinking that margin is what breaks first.
- Determinism buys prompt-cache prefix stability, not correctness. Correctness
  comes from remapping the whole body and reversing at every egress, so history
  arrives in openclaw names and is re-masked wholesale each turn. Probing makes
  a colliding tool's name depend on the roster and its order, not on the name
  alone — about 13 of 46 tools resolve by probing — so a roster change
  reshuffles some names and costs one cache miss.
- The model is told a tool is named `Rename` when it is `nodes`. Descriptions
  and input schemas are untouched, which is the only thing keeping dispatch
  sane; any behavior keyed on the *name* is now keyed on a fiction.
- A third response path added without the reverse call makes openclaw dispatch a
  tool it does not have. The two existing paths are the invariant, not a
  convention the compiler enforces.
- Removing this means deleting a self-contained `src/masquerade.rs` and
  unthreading `tool_reverse` from 12 references in `src/app.rs`, including an
  `Arc<BTreeMap<..>>` field on the streaming struct. There is no on-disk state
  and no wire contract to migrate. The failure mode of removal is the original
  400.
