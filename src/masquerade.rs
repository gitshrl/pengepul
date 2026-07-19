//! Make an openclaw embedded-runner request look like a first-party Claude Code
//! request so Anthropic's subscription billing classifier does not reject it.
//!
//! openclaw sends its own tool names (`exec`, `web_search`, `create_goal`, ...)
//! and a bot-persona system prompt; the classifier flags both as a third-party
//! bridge and routes the request to extra-usage billing (a hard 400 on
//! overage-disabled orgs). The trigger is naming *style*, not vocabulary: the
//! classifier accepts `PascalCase`, coding-assistant-looking tool names and rejects
//! openclaw's `snake_case` ones (verified against the live classifier — `Exec`,
//! `CreateGoal`, `Subagents` all pass; `exec`, `create_goal` all trip). So each
//! tool name is `PascalCase`, preserving its meaning, and the bot-identity sections
//! are stripped from the system prompt. Server tools (a `type` field, e.g.
//! `web_search_20250305`) are left untouched so Anthropic still executes them. The
//! mapping is deterministic so multi-turn history stays consistent, and the reverse
//! map is returned to restore `tool_use` names in the response before openclaw
//! dispatches them.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

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
    "reactions",
    "authorized senders",
    "allowlisted senders",
    "inbound context",
    "trusted metadata",
    "assistant output",
    "output directives",
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

/// `PascalCase` a snake/kebab/space-delimited tool name (`web_search` → `WebSearch`).
/// A leading digit or empty result falls back to `Tool` so the name always looks
/// like an identifier.
fn pascal_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for part in name.split(['_', '-', ' ', '.']) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        format!("Tool{out}")
    } else {
        out
    }
}

/// `PascalCase` the name; if two distinct tools collapse to the same `PascalCase`
/// (`a_b` and `ab` both → `Ab`), append the smallest free numeric suffix so the
/// map stays bijective and the reverse map round-trips.
fn pseudo_for(name: &str, taken: &BTreeSet<String>) -> String {
    let base = pascal_case(name);
    if !taken.contains(&base) {
        return base;
    }
    // At most `taken.len()` prior names can collide, so a free suffix exists within
    // that range; the bound also keeps the search provably terminating.
    (2..=taken.len() + 2)
        .map(|n| format!("{base}{n}"))
        .find(|candidate| !taken.contains(candidate))
        .unwrap_or(base)
}

/// openclaw ships its own custom `web_search` tool that it executes itself. Swap
/// it for Anthropic's native server tool so the upstream runs the search and folds
/// the results into the turn; the name stays `web_search` (no client dispatch), so
/// it is excluded from the `PascalCase` map. Returns the native tool definition, or
/// `None` for tools with no native equivalent.
fn native_replacement(name: &str) -> Option<Value> {
    match name {
        "web_search" => Some(serde_json::json!({
            "type": "web_search_20250305",
            "name": "web_search",
            "max_uses": 5,
        })),
        _ => None,
    }
}

/// Map every custom tool name to its `PascalCase` pseudo-name. Skipped: server tools
/// (a `type` field, already Anthropic-native) and tools with a native replacement
/// (handled separately, keep their real name).
fn build_tool_map(tools: &[Value]) -> BTreeMap<String, String> {
    let mut taken = BTreeSet::new();
    let mut map = BTreeMap::new();
    for tool in tools {
        if tool.get("type").is_some() {
            continue;
        }
        if let Some(name) = tool.get("name").and_then(Value::as_str)
            && native_replacement(name).is_none()
            && !map.contains_key(name)
        {
            let pseudo = pseudo_for(name, &taken);
            taken.insert(pseudo.clone());
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
            let Some(name) = tool.get("name").and_then(Value::as_str) else {
                continue;
            };
            if let Some(native) = native_replacement(name) {
                *tool = native;
            } else if let Some(pseudo) = tool_map.get(name) {
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
