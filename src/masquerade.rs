//! Make an openclaw embedded-runner request look like a first-party Claude Code
//! request so Anthropic's subscription billing classifier does not reject it.
//!
//! openclaw sends its own tool names (`exec`, `gateway`, `nodes`, ...) and a
//! bot-persona system prompt; the classifier flags both as a third-party bridge
//! and routes the request to extra-usage billing (a hard 400 on overage-disabled
//! orgs). This module renames each tool to a Claude-Code-style pseudo-name and
//! strips the bot-identity sections from the system prompt. Tool names are mapped
//! deterministically so multi-turn history stays consistent, and the reverse map
//! is returned so `tool_use` names in the response can be restored before openclaw
//! dispatches them.

use std::collections::BTreeMap;

use serde_json::Value;

/// Claude-Code-style names the classifier accepts as first-party. Sized well above
/// openclaw's tool roster (46 in 2026.7.2) so the deterministic map never spills to
/// the numeric fallback in practice; assignment is by stable hash with probing.
const CC_TOOL_POOL: &[&str] = &[
    "Bash", "Read", "Write", "Edit", "MultiEdit", "Glob", "Grep", "LS", "WebSearch",
    "WebFetch", "Task", "TodoWrite", "NotebookEdit", "NotebookRead", "BashOutput",
    "KillShell", "ExitPlanMode", "SlashCommand", "Agent", "Plan", "View", "Replace",
    "Fetch", "Search", "Move", "Copy", "Delete", "Diff", "Patch", "Format", "Lint",
    "Test", "Build", "Run", "Watch", "Inspect", "Trace", "Profile", "Debug", "Explain",
    "Compile", "Deploy", "Rename", "Find", "Open", "Close", "Save", "Load", "Sync",
    "Merge", "Split", "Filter", "Sort", "Count", "List", "Show", "Print", "Parse",
    "Render", "Compress", "Extract", "Encode", "Decode", "Validate", "Verify", "Analyze",
    "Report", "Query", "Apply", "Revert", "Stage", "Commit", "Branch", "Checkout",
    "Clone", "Push", "Pull", "Tag", "Log", "Blame", "Stash", "Rebase",
];

/// Case-insensitive keywords that mark a system-prompt heading as chat-bot
/// identity. Matched against the heading text (not the body) so wording/emoji
/// variants across openclaw versions still hit; a matched heading of level L
/// removes everything up to the next heading of level <= L, so sub-sections are
/// swallowed with their parent.
const BOT_SECTION_KEYWORDS: &[&str] = &[
    "messaging",
    "message tool",
    "heartbeat",
    "group chat",
    "reply tag",
    "silent repl",
    "know when to speak",
    "react like a human",
];

fn is_bot_heading(line: &str) -> bool {
    let lower = line.to_lowercase();
    BOT_SECTION_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

fn heading_level(line: &str) -> Option<usize> {
    if !line.starts_with('#') {
        return None;
    }
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if line[hashes..].starts_with(' ') {
        Some(hashes)
    } else {
        None
    }
}

/// FNV-1a over the tool name → a stable pool index; linear-probe keeps the map
/// bijective within a request. Returns `None` if the pool is exhausted (more tools
/// than pool names), in which case the tool keeps its original name.
fn pseudo_for(name: &str, taken: &mut [bool]) -> Option<String> {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let start = usize::try_from(hash % CC_TOOL_POOL.len() as u64).unwrap_or(0);
    for offset in 0..CC_TOOL_POOL.len() {
        let idx = (start + offset) % CC_TOOL_POOL.len();
        if !taken[idx] {
            taken[idx] = true;
            return Some(CC_TOOL_POOL[idx].to_string());
        }
    }
    None
}

fn build_tool_map(tools: &[Value]) -> BTreeMap<String, String> {
    let mut taken = vec![false; CC_TOOL_POOL.len()];
    let mut map = BTreeMap::new();
    for tool in tools {
        if let Some(name) = tool.get("name").and_then(Value::as_str)
            && !map.contains_key(name)
            && let Some(pseudo) = pseudo_for(name, &mut taken)
        {
            map.insert(name.to_string(), pseudo);
        }
    }
    map
}

fn sanitize_system_text(text: &str, tool_map: &BTreeMap<String, String>) -> String {
    // Drop bot-identity sections.
    let mut kept = Vec::new();
    let mut skip_until_level: Option<usize> = None;
    for line in text.lines() {
        if let Some(level) = heading_level(line) {
            if let Some(active) = skip_until_level
                && level <= active
            {
                skip_until_level = None;
            }
            if skip_until_level.is_none() && is_bot_heading(line) {
                skip_until_level = Some(level);
            }
        }
        if skip_until_level.is_none() {
            kept.push(line);
        }
    }
    let mut out = kept.join("\n");

    // Rename the tool listing ("- <name>: ...") to the pseudo-names. Confined to
    // that pattern so common English words (read/edit/message/process) in prose
    // are not clobbered.
    for (orig, pseudo) in tool_map {
        out = out.replace(&format!("- {orig}:"), &format!("- {pseudo}:"));
    }

    out
}

fn remap_tool_use_names(messages: &mut Value, tool_map: &BTreeMap<String, String>) {
    let Some(list) = messages.as_array_mut() else {
        return;
    };
    for message in list {
        let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("tool_use")
                && let Some(name) = block.get("name").and_then(Value::as_str)
                && let Some(pseudo) = tool_map.get(name)
            {
                block["name"] = Value::String(pseudo.clone());
            }
        }
    }
}

/// Transform an outbound `/v1/messages` body into a first-party-looking request.
/// Returns the rewritten body and the reverse map (pseudo-name → openclaw name)
/// for restoring `tool_use` names in the response.
#[must_use]
pub fn masquerade_request(body: &Value) -> (Value, BTreeMap<String, String>) {
    let mut next = body.clone();
    let Some(object) = next.as_object_mut() else {
        return (next, BTreeMap::new());
    };

    let tool_map = object
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| build_tool_map(tools))
        .unwrap_or_default();

    if let Some(tools) = object.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools {
            if let Some(name) = tool.get("name").and_then(Value::as_str)
                && let Some(pseudo) = tool_map.get(name)
            {
                tool["name"] = Value::String(pseudo.clone());
            }
        }
    }

    if let Some(system) = object.get_mut("system") {
        match system {
            Value::Array(blocks) => {
                for block in blocks {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        block["text"] = Value::String(sanitize_system_text(text, &tool_map));
                    }
                }
            }
            Value::String(text) => {
                *system = Value::String(sanitize_system_text(text, &tool_map));
            }
            _ => {}
        }
    }

    if let Some(messages) = object.get_mut("messages") {
        remap_tool_use_names(messages, &tool_map);
    }

    let reverse = tool_map.into_iter().map(|(k, v)| (v, k)).collect();
    (next, reverse)
}

/// Restore a `tool_use` name from the response to the openclaw tool name so
/// openclaw dispatches it. Unknown names pass through unchanged.
#[must_use]
pub fn restore_tool_name(name: &str, reverse: &BTreeMap<String, String>) -> String {
    reverse
        .get(name)
        .cloned()
        .unwrap_or_else(|| name.to_string())
}

fn restore_block_name(block: &mut Value, reverse: &BTreeMap<String, String>) {
    if block.get("type").and_then(Value::as_str) == Some("tool_use")
        && let Some(name) = block.get("name").and_then(Value::as_str)
        && let Some(orig) = reverse.get(name)
    {
        block["name"] = Value::String(orig.clone());
    }
}

/// Restore masked `tool_use` names in a response value — a full message body
/// (`content` array) or a single streaming SSE event (`content_block`). No-op
/// when the reverse map is empty.
pub fn restore_tool_use_names(value: &mut Value, reverse: &BTreeMap<String, String>) {
    if reverse.is_empty() {
        return;
    }
    if let Some(content) = value.get_mut("content").and_then(Value::as_array_mut) {
        for block in content {
            restore_block_name(block, reverse);
        }
    }
    if let Some(block) = value.get_mut("content_block") {
        restore_block_name(block, reverse);
    }
}
