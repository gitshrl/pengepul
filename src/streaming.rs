use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde_json::{Value, json};

use crate::types::UsageData;

#[derive(Debug, Clone)]
pub struct ChatStreamState {
    model: String,
    id: String,
    tool_block_indexes: BTreeMap<i64, i64>,
    tool_argument_delta_indexes: BTreeSet<i64>,
    has_tool_call: bool,
    usage: UsageData,
}

impl ChatStreamState {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            id: format!("chatcmpl_{}", uuid::Uuid::new_v4().simple()),
            tool_block_indexes: BTreeMap::new(),
            tool_argument_delta_indexes: BTreeSet::new(),
            has_tool_call: false,
            usage: UsageData::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResponsesStreamState {
    id: String,
    next_output_index: i64,
    block_output_indexes: BTreeMap<i64, i64>,
    tool_calls: BTreeMap<i64, FunctionCall>,
    usage: UsageData,
}

impl ResponsesStreamState {
    #[must_use]
    pub fn new(_model: impl Into<String>) -> Self {
        Self {
            id: format!("resp_{}", uuid::Uuid::new_v4().simple()),
            next_output_index: 0,
            block_output_indexes: BTreeMap::new(),
            tool_calls: BTreeMap::new(),
            usage: UsageData::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AnthropicStreamState {
    model: String,
    message_id: String,
    active_block: Option<BlockType>,
    next_index: i64,
    tool_call_blocks: BTreeMap<i64, i64>,
    tool_argument_delta_indexes: BTreeSet<i64>,
    has_tool_use: bool,
}

impl AnthropicStreamState {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            message_id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            active_block: None,
            next_index: 0,
            tool_call_blocks: BTreeMap::new(),
            tool_argument_delta_indexes: BTreeSet::new(),
            has_tool_use: false,
        }
    }
}

#[derive(Debug, Clone)]
struct FunctionCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Default)]
struct ResponsesDrain {
    text_out: String,
    reasoning_out: String,
    tool_calls: BTreeMap<String, FunctionCall>,
    output_items: Vec<Value>,
    completed_response: Option<Value>,
    upstream_error: Option<String>,
    status: String,
    usage: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockType {
    Text,
    Thinking,
    ToolUse,
    ServerToolUse,
}

/// Parse complete SSE events from byte chunks.
///
/// # Errors
///
/// Returns an error when the chunks are not valid UTF-8.
pub fn parse_sse_events(chunks: &[&[u8]]) -> Result<Vec<(String, String)>> {
    let mut buffer = String::new();
    for chunk in chunks {
        buffer.push_str(std::str::from_utf8(chunk)?);
    }

    let mut events = Vec::new();
    while let Some((index, length)) = find_sse_separator(&buffer) {
        let raw = buffer[..index].to_string();
        buffer = buffer[index + length..].to_string();
        if let Some(event) = parse_event(&raw) {
            events.push(event);
        }
    }
    if !buffer.trim().is_empty()
        && let Some(event) = parse_event(&buffer)
    {
        events.push(event);
    }
    Ok(events)
}

/// Drain complete SSE events from an incremental byte buffer.
///
/// # Errors
///
/// Returns an error when a complete event is not valid UTF-8.
pub fn drain_complete_sse_events(buffer: &mut Vec<u8>) -> Result<Vec<(String, String)>> {
    let mut events = Vec::new();
    while let Some((index, length)) = find_sse_separator_bytes(buffer) {
        let raw = buffer[..index].to_vec();
        buffer.drain(..index + length);
        let raw = std::str::from_utf8(&raw)?;
        if let Some(event) = parse_event(raw) {
            events.push(event);
        }
    }
    Ok(events)
}

/// Parse a final unterminated SSE event from a byte buffer.
///
/// # Errors
///
/// Returns an error when the final event is not valid UTF-8.
pub fn finish_sse_events(buffer: &mut Vec<u8>) -> Result<Vec<(String, String)>> {
    if buffer.iter().all(u8::is_ascii_whitespace) {
        buffer.clear();
        return Ok(Vec::new());
    }
    let raw = std::str::from_utf8(buffer)?;
    let events = parse_event(raw).into_iter().collect();
    buffer.clear();
    Ok(events)
}

#[must_use]
/// Encode one Server-Sent Event chunk.
///
/// # Panics
///
/// Panics only if serializing an in-memory `serde_json::Value` into a `String` fails.
pub fn sse(data: &Value, event: Option<&str>) -> String {
    let payload = if let Some(raw) = data.as_str() {
        raw.to_string()
    } else {
        serde_json::to_string(data).expect("SSE payload must serialize")
    };
    let mut lines = Vec::new();
    if let Some(event) = event {
        lines.push(format!("event: {event}"));
    }
    for line in payload.lines() {
        lines.push(format!("data: {line}"));
    }
    if payload.is_empty() {
        lines.push("data: ".to_string());
    }
    format!("{}\n\n", lines.join("\n"))
}

/// Drain Responses API SSE bytes into one Responses JSON payload.
///
/// # Errors
///
/// Returns an error when the SSE bytes are not valid UTF-8.
pub fn responses_sse_to_payload(chunks: &[&[u8]], model: &str) -> Result<Value> {
    let events = parse_sse_events(chunks)?;
    let mut drain = ResponsesDrain {
        status: "completed".to_string(),
        usage: json!({}),
        ..ResponsesDrain::default()
    };
    for (event, raw) in events {
        if raw == "[DONE]" {
            break;
        }
        let Ok(data) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        update_responses_drain(&mut drain, &event, &data);
    }
    Ok(response_payload_from_drain(&drain, model))
}

fn update_responses_drain(drain: &mut ResponsesDrain, event: &str, data: &Value) {
    match event {
        "response.output_text.delta" | "response.refusal.delta" => {
            drain
                .text_out
                .push_str(data.get("delta").and_then(Value::as_str).unwrap_or(""));
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            drain
                .reasoning_out
                .push_str(data.get("delta").and_then(Value::as_str).unwrap_or(""));
        }
        "response.output_item.done" => {
            let item = data.get("item").unwrap_or(&Value::Null);
            match item.get("type").and_then(Value::as_str) {
                Some("function_call") => {
                    store_function_call(
                        drain,
                        drain_key(item, data),
                        function_call_from_item(item),
                    );
                }
                Some("message" | "reasoning") | None => {}
                Some(_) => drain.output_items.push(item.clone()),
            }
        }
        "response.output_item.added" => {
            let item = data.get("item").unwrap_or(&Value::Null);
            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                store_function_call(drain, drain_key(item, data), function_call_from_item(item));
            }
        }
        "response.function_call_arguments.delta" => {
            let key = data
                .get("item_id")
                .or_else(|| data.get("output_index"))
                .map(value_to_key)
                .unwrap_or_default();
            let call = drain.tool_calls.entry(key).or_insert(FunctionCall {
                id: String::new(),
                name: String::new(),
                arguments: String::new(),
            });
            call.arguments
                .push_str(data.get("delta").and_then(Value::as_str).unwrap_or(""));
        }
        "response.function_call_arguments.done" => {
            let item = data.get("item").unwrap_or(data);
            store_function_call(drain, drain_key(item, data), function_call_from_item(item));
        }
        "response.completed" => {
            let response = data.get("response").unwrap_or(data);
            drain.completed_response = Some(response.clone());
            drain.status = response
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("completed")
                .to_string();
            drain.usage = response
                .get("usage")
                .cloned()
                .unwrap_or_else(|| drain.usage.clone());
        }
        "response.incomplete" => {
            let response = data.get("response").unwrap_or(data);
            drain.status = "incomplete".to_string();
            drain.usage = response
                .get("usage")
                .cloned()
                .unwrap_or_else(|| drain.usage.clone());
        }
        "response.failed" => {
            let error = data.get("error").unwrap_or(&Value::Null);
            drain.upstream_error = error
                .get("message")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| error.as_str().map(ToOwned::to_owned));
        }
        _ => {}
    }
}

fn response_payload_from_drain(drain: &ResponsesDrain, model: &str) -> Value {
    if let Some(completed_response) = &drain.completed_response {
        let mut payload = completed_response.clone();
        if payload.get("output").is_none_or(is_empty) {
            set_value_field(
                &mut payload,
                "output",
                Value::Array(output_from_drain(drain)),
            );
        }
        if payload.get("output_text").is_none() && !drain.text_out.is_empty() {
            set_value_field(
                &mut payload,
                "output_text",
                Value::String(drain.text_out.clone()),
            );
        }
        return payload;
    }
    if let Some(upstream_error) = &drain.upstream_error
        && drain.text_out.is_empty()
        && drain.reasoning_out.is_empty()
        && drain.tool_calls.is_empty()
        && drain.output_items.is_empty()
    {
        return json!({"error": {"message": upstream_error, "type": "upstream_error"}});
    }

    json!({
        "id": format!("resp_{}", uuid::Uuid::new_v4().simple()),
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "status": drain.status,
        "model": model,
        "output": output_from_drain(drain),
        "output_text": drain.text_out,
        "usage": drain.usage
    })
}

fn output_from_drain(drain: &ResponsesDrain) -> Vec<Value> {
    let mut output = drain.output_items.clone();
    if !drain.reasoning_out.is_empty() {
        output.push(json!({
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": drain.reasoning_out}]
        }));
    }
    if !drain.text_out.is_empty() {
        output.push(json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": drain.text_out}]
        }));
    }
    output.extend(drain.tool_calls.values().map(|call| {
        json!({
            "type": "function_call",
            "call_id": call.id,
            "name": call.name,
            "arguments": call.arguments
        })
    }));
    output
}

fn store_function_call(drain: &mut ResponsesDrain, key: String, call: FunctionCall) {
    drain.tool_calls.insert(key, call);
}

fn function_call_from_item(item: &Value) -> FunctionCall {
    FunctionCall {
        id: item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        name: item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        arguments: item
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    }
}

fn drain_key(item: &Value, data: &Value) -> String {
    item.get("id")
        .or_else(|| data.get("item_id"))
        .or_else(|| data.get("output_index"))
        .map(value_to_key)
        .unwrap_or_default()
}

fn value_to_key(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), ToOwned::to_owned)
}

fn set_value_field(target: &mut Value, key: &str, value: Value) {
    if let Some(object) = target.as_object_mut() {
        object.insert(key.to_string(), value);
    }
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn anthropic_sse_to_chat(
    event: &str,
    data: &Value,
    state: &mut ChatStreamState,
) -> Vec<String> {
    match event {
        "message_start" => {
            if let Some(usage) = data.get("message").and_then(|message| message.get("usage")) {
                update_anthropic_usage(&mut state.usage, usage);
            }
            vec![sse(
                &json!({
                    "id": state.id,
                    "object": "chat.completion.chunk",
                    "created": chrono::Utc::now().timestamp(),
                    "model": state.model,
                    "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": Value::Null}]
                }),
                None,
            )]
        }
        "content_block_start" => {
            let block = data.get("content_block").unwrap_or(&Value::Null);
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                return Vec::new();
            }
            let block_index = int_field(data, "index");
            let tool_index = i64::try_from(state.tool_block_indexes.len()).unwrap_or(0);
            state.tool_block_indexes.insert(block_index, tool_index);
            state.has_tool_call = true;
            vec![chat_delta(
                state,
                &json!({
                    "tool_calls": [{
                        "index": tool_index,
                        "id": block.get("id").cloned().unwrap_or(Value::Null),
                        "type": "function",
                        "function": {
                            "name": block.get("name").cloned().unwrap_or(Value::Null),
                            "arguments": ""
                        }
                    }]
                }),
            )]
        }
        "content_block_delta" => {
            let delta = data.get("delta").unwrap_or(&Value::Null);
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => vec![chat_delta(
                    state,
                    &json!({"content": delta.get("text").and_then(Value::as_str).unwrap_or("")}),
                )],
                Some("thinking_delta") => vec![chat_delta(
                    state,
                    &json!({"reasoning_content": delta.get("thinking").and_then(Value::as_str).unwrap_or("")}),
                )],
                Some("input_json_delta") => {
                    let block_index = int_field(data, "index");
                    let tool_index = state
                        .tool_block_indexes
                        .get(&block_index)
                        .copied()
                        .unwrap_or(block_index);
                    vec![chat_delta(
                        state,
                        &json!({
                            "tool_calls": [{
                                "index": tool_index,
                                "function": {
                                    "arguments": delta.get("partial_json").and_then(Value::as_str).unwrap_or("")
                                }
                            }]
                        }),
                    )]
                }
                _ => Vec::new(),
            }
        }
        "message_delta" => {
            if let Some(usage) = data.get("usage") {
                update_anthropic_usage(&mut state.usage, usage);
            }
            let Some(stop_reason) = data
                .get("delta")
                .and_then(|delta| delta.get("stop_reason"))
                .and_then(Value::as_str)
            else {
                return Vec::new();
            };
            vec![chat_done(state, anthropic_stop_reason_to_chat(stop_reason))]
        }
        "message_stop" => vec![
            sse(
                &json!({
                    "id": state.id,
                    "object": "chat.completion.chunk",
                    "created": chrono::Utc::now().timestamp(),
                    "model": state.model,
                    "choices": [],
                    "usage": chat_usage(&state.usage)
                }),
                None,
            ),
            "data: [DONE]\n\n".to_string(),
        ],
        _ => Vec::new(),
    }
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn responses_sse_to_anthropic(
    event: &str,
    data: &Value,
    state: &mut AnthropicStreamState,
) -> Vec<String> {
    match event {
        "response.created" => vec![sse(
            &json!({
                "type": "message_start",
                "message": {
                    "id": state.message_id,
                    "type": "message",
                    "role": "assistant",
                    "model": state.model,
                    "content": [],
                    "stop_reason": Value::Null,
                    "stop_sequence": Value::Null,
                    "usage": {"input_tokens": 0, "output_tokens": 0}
                }
            }),
            Some("message_start"),
        )],
        "response.output_text.delta" | "response.refusal.delta" => {
            let mut chunks = ensure_anthropic_block(state, BlockType::Text);
            chunks.push(sse(
                &json!({
                    "type": "content_block_delta",
                    "index": state.next_index - 1,
                    "delta": {"type": "text_delta", "text": data.get("delta").and_then(Value::as_str).unwrap_or("")}
                }),
                Some("content_block_delta"),
            ));
            chunks
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            let mut chunks = ensure_anthropic_block(state, BlockType::Thinking);
            chunks.push(sse(
                &json!({
                    "type": "content_block_delta",
                    "index": state.next_index - 1,
                    "delta": {"type": "thinking_delta", "thinking": data.get("delta").and_then(Value::as_str).unwrap_or("")}
                }),
                Some("content_block_delta"),
            ));
            chunks
        }
        "response.output_item.added" => {
            let item = data.get("item").unwrap_or(&Value::Null);
            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                let mut chunks = stop_active_block(state);
                let index = state.next_index;
                state.next_index += 1;
                state.active_block = Some(BlockType::ToolUse);
                state.has_tool_use = true;
                let output_index = int_field(data, "output_index");
                state.tool_call_blocks.insert(output_index, index);
                chunks.push(sse(
                    &json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "tool_use",
                            "id": item.get("call_id").or_else(|| item.get("id")).cloned().unwrap_or(Value::Null),
                            "name": item.get("name").cloned().unwrap_or(Value::Null),
                            "input": {}
                        }
                    }),
                    Some("content_block_start"),
                ));
                return chunks;
            }
            if item.get("type").and_then(Value::as_str) == Some("web_search_call") {
                return start_anthropic_server_tool_use(state, data);
            }
            Vec::new()
        }
        "response.output_item.done" => {
            let item = data.get("item").unwrap_or(&Value::Null);
            if item.get("type").and_then(Value::as_str) == Some("web_search_call") {
                return start_anthropic_server_tool_use(state, data);
            }
            Vec::new()
        }
        "response.function_call_arguments.delta" => {
            let output_index = int_field(data, "output_index");
            state.tool_argument_delta_indexes.insert(output_index);
            let block_index = state
                .tool_call_blocks
                .get(&output_index)
                .copied()
                .unwrap_or(state.next_index - 1);
            vec![sse(
                &json!({
                    "type": "content_block_delta",
                    "index": block_index,
                    "delta": {"type": "input_json_delta", "partial_json": data.get("delta").and_then(Value::as_str).unwrap_or("")}
                }),
                Some("content_block_delta"),
            )]
        }
        "response.function_call_arguments.done" => {
            let output_index = int_field(data, "output_index");
            if state.tool_argument_delta_indexes.contains(&output_index) {
                return Vec::new();
            }
            let item = data.get("item").unwrap_or(data);
            let block_index = state
                .tool_call_blocks
                .get(&output_index)
                .copied()
                .unwrap_or(state.next_index - 1);
            vec![sse(
                &json!({
                    "type": "content_block_delta",
                    "index": block_index,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": item.get("arguments").and_then(Value::as_str).unwrap_or("")
                    }
                }),
                Some("content_block_delta"),
            )]
        }
        "response.completed" | "response.incomplete" => {
            let mut chunks = stop_active_block(state);
            let response = data.get("response").unwrap_or(data);
            let usage = response.get("usage").unwrap_or(&Value::Null);
            chunks.push(sse(
                &json!({
                    "type": "message_delta",
                    "delta": {
                        "stop_reason": if state.has_tool_use {
                            "tool_use"
                        } else if event == "response.incomplete" {
                            "max_tokens"
                        } else {
                            "end_turn"
                        },
                        "stop_sequence": Value::Null
                    },
                    "usage": {"output_tokens": int_field(usage, "output_tokens")}
                }),
                Some("message_delta"),
            ));
            chunks.push(sse(&json!({"type": "message_stop"}), Some("message_stop")));
            chunks
        }
        _ => Vec::new(),
    }
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn anthropic_sse_to_responses(
    event: &str,
    data: &Value,
    state: &mut ResponsesStreamState,
    model: &str,
) -> Vec<String> {
    match event {
        "message_start" => {
            if let Some(usage) = data.get("message").and_then(|message| message.get("usage")) {
                update_anthropic_usage(&mut state.usage, usage);
            }
            vec![sse(
                &json!({
                    "type": "response.created",
                    "response": {
                        "id": state.id,
                        "object": "response",
                        "created_at": chrono::Utc::now().timestamp(),
                        "status": "in_progress",
                        "model": model
                    }
                }),
                Some("response.created"),
            )]
        }
        "content_block_start" => {
            let block = data.get("content_block").unwrap_or(&Value::Null);
            let block_index = int_field(data, "index");
            let output_index = responses_output_index(state, block_index);
            match block.get("type").and_then(Value::as_str) {
                Some("text") => vec![
                    sse(
                        &json!({
                            "type": "response.output_item.added",
                            "output_index": output_index,
                            "item": {"type": "message", "role": "assistant", "content": []}
                        }),
                        Some("response.output_item.added"),
                    ),
                    sse(
                        &json!({
                            "type": "response.content_part.added",
                            "item_id": state.id,
                            "output_index": output_index,
                            "content_index": 0,
                            "part": {"type": "output_text", "text": ""}
                        }),
                        Some("response.content_part.added"),
                    ),
                ],
                Some("thinking") => vec![sse(
                    &json!({
                        "type": "response.output_item.added",
                        "output_index": output_index,
                        "item": {"type": "reasoning", "summary": []}
                    }),
                    Some("response.output_item.added"),
                )],
                Some("tool_use") => {
                    let call = FunctionCall {
                        id: block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_string(),
                        name: block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        arguments: block
                            .get("input")
                            .filter(|input| !input.as_object().is_some_and(MapExt::is_empty))
                            .map_or_else(String::new, |input| {
                                serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string())
                            }),
                    };
                    state.tool_calls.insert(block_index, call.clone());
                    vec![sse(
                        &json!({
                            "type": "response.output_item.added",
                            "output_index": output_index,
                            "item": response_function_item(&call)
                        }),
                        Some("response.output_item.added"),
                    )]
                }
                Some("server_tool_use")
                    if block.get("name").and_then(Value::as_str) == Some("web_search") =>
                {
                    vec![sse(
                        &json!({
                            "type": "response.output_item.added",
                            "output_index": output_index,
                            "item": {
                                "type": "web_search_call",
                                "id": block.get("id").cloned().unwrap_or(Value::Null),
                                "status": "completed",
                                "action": {
                                    "type": "search",
                                    "query": block.get("input").and_then(|input| input.get("query")).and_then(Value::as_str).unwrap_or("")
                                }
                            }
                        }),
                        Some("response.output_item.added"),
                    )]
                }
                _ => Vec::new(),
            }
        }
        "content_block_delta" => {
            let delta = data.get("delta").unwrap_or(&Value::Null);
            let block_index = int_field(data, "index");
            let output_index = responses_output_index(state, block_index);
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => vec![sse(
                    &json!({
                        "type": "response.output_text.delta",
                        "output_index": output_index,
                        "content_index": 0,
                        "delta": delta.get("text").and_then(Value::as_str).unwrap_or("")
                    }),
                    Some("response.output_text.delta"),
                )],
                Some("thinking_delta") => vec![sse(
                    &json!({
                        "type": "response.reasoning_text.delta",
                        "output_index": output_index,
                        "delta": delta.get("thinking").and_then(Value::as_str).unwrap_or("")
                    }),
                    Some("response.reasoning_text.delta"),
                )],
                Some("input_json_delta") => {
                    let call = state.tool_calls.entry(block_index).or_insert(FunctionCall {
                        id: block_index.to_string(),
                        name: String::new(),
                        arguments: String::new(),
                    });
                    let partial = delta
                        .get("partial_json")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    call.arguments.push_str(partial);
                    vec![sse(
                        &json!({
                            "type": "response.function_call_arguments.delta",
                            "item_id": call.id,
                            "output_index": output_index,
                            "delta": partial
                        }),
                        Some("response.function_call_arguments.delta"),
                    )]
                }
                _ => Vec::new(),
            }
        }
        "content_block_stop" => {
            let block_index = int_field(data, "index");
            let Some(call) = state.tool_calls.get(&block_index).cloned() else {
                return Vec::new();
            };
            let output_index = responses_output_index(state, block_index);
            let item = response_function_item(&call);
            vec![
                sse(
                    &json!({
                        "type": "response.function_call_arguments.done",
                        "output_index": output_index,
                        "item": item
                    }),
                    Some("response.function_call_arguments.done"),
                ),
                sse(
                    &json!({
                        "type": "response.output_item.done",
                        "output_index": output_index,
                        "item": item
                    }),
                    Some("response.output_item.done"),
                ),
            ]
        }
        "message_delta" => {
            if let Some(usage) = data.get("usage") {
                update_anthropic_usage(&mut state.usage, usage);
            }
            Vec::new()
        }
        "message_stop" => vec![sse(
            &json!({
                "type": "response.completed",
                "response": {
                    "id": state.id,
                    "object": "response",
                    "created_at": chrono::Utc::now().timestamp(),
                    "status": "completed",
                    "model": model,
                    "usage": responses_usage(&state.usage)
                }
            }),
            Some("response.completed"),
        )],
        _ => Vec::new(),
    }
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn responses_sse_to_chat(
    event: &str,
    data: &Value,
    state: &mut ChatStreamState,
) -> Vec<String> {
    match event {
        "response.created" => vec![sse(
            &json!({
                "id": state.id,
                "object": "chat.completion.chunk",
                "created": chrono::Utc::now().timestamp(),
                "model": state.model,
                "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": Value::Null}]
            }),
            None,
        )],
        "response.output_item.added" => {
            let item = data.get("item").unwrap_or(&Value::Null);
            if item.get("type").and_then(Value::as_str) != Some("function_call") {
                return Vec::new();
            }
            state.has_tool_call = true;
            vec![chat_delta(
                state,
                &json!({
                    "tool_calls": [{
                        "index": int_field(data, "output_index"),
                        "id": item.get("call_id").or_else(|| item.get("id")).cloned().unwrap_or(Value::Null),
                        "type": "function",
                        "function": {
                            "name": item.get("name").cloned().unwrap_or(Value::Null),
                            "arguments": item.get("arguments").cloned().unwrap_or_else(|| json!(""))
                        }
                    }]
                }),
            )]
        }
        "response.function_call_arguments.delta" => {
            state.has_tool_call = true;
            let output_index = int_field(data, "output_index");
            state.tool_argument_delta_indexes.insert(output_index);
            vec![chat_delta(
                state,
                &json!({
                    "tool_calls": [{
                        "index": output_index,
                        "function": {"arguments": data.get("delta").cloned().unwrap_or_else(|| json!(""))}
                    }]
                }),
            )]
        }
        "response.function_call_arguments.done" => {
            state.has_tool_call = true;
            let output_index = int_field(data, "output_index");
            if state.tool_argument_delta_indexes.contains(&output_index) {
                return Vec::new();
            }
            let item = data.get("item").unwrap_or(data);
            vec![chat_delta(
                state,
                &json!({
                    "tool_calls": [{
                        "index": output_index,
                        "function": {"arguments": item.get("arguments").and_then(Value::as_str).unwrap_or("")}
                    }]
                }),
            )]
        }
        "response.output_text.delta" | "response.refusal.delta" => vec![chat_delta(
            state,
            &json!({"content": data.get("delta").and_then(Value::as_str).unwrap_or("")}),
        )],
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            vec![chat_delta(
                state,
                &json!({"reasoning_content": data.get("delta").and_then(Value::as_str).unwrap_or("")}),
            )]
        }
        "response.completed" | "response.incomplete" => {
            let mut chunks = vec![chat_done(
                state,
                if state.has_tool_call {
                    "tool_calls"
                } else if event == "response.incomplete" {
                    "length"
                } else {
                    "stop"
                },
            )];
            let response = data.get("response").unwrap_or(data);
            if let Some(usage) = response.get("usage") {
                update_responses_usage(&mut state.usage, usage);
                chunks.push(sse(
                    &json!({
                        "id": state.id,
                        "object": "chat.completion.chunk",
                        "created": chrono::Utc::now().timestamp(),
                        "model": state.model,
                        "choices": [],
                        "usage": chat_usage(&state.usage)
                    }),
                    None,
                ));
            }
            chunks.push("data: [DONE]\n\n".to_string());
            chunks
        }
        _ => Vec::new(),
    }
}

fn find_sse_separator(buffer: &str) -> Option<(usize, usize)> {
    ["\r\n\r\n", "\n\n", "\r\r"]
        .into_iter()
        .filter_map(|separator| buffer.find(separator).map(|index| (index, separator.len())))
        .min_by_key(|(index, _)| *index)
}

fn find_sse_separator_bytes(buffer: &[u8]) -> Option<(usize, usize)> {
    [
        b"\r\n\r\n".as_slice(),
        b"\n\n".as_slice(),
        b"\r\r".as_slice(),
    ]
    .into_iter()
    .filter_map(|separator| {
        buffer
            .windows(separator.len())
            .position(|window| window == separator)
            .map(|index| (index, separator.len()))
    })
    .min_by_key(|(index, _)| *index)
}

fn parse_event(raw: &str) -> Option<(String, String)> {
    let mut event = "message".to_string();
    let mut data = Vec::new();
    for line in raw.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value).to_string());
        }
    }
    (!data.is_empty()).then(|| (event, data.join("\n")))
}

fn ensure_anthropic_block(state: &mut AnthropicStreamState, block_type: BlockType) -> Vec<String> {
    if state.active_block == Some(block_type) {
        return Vec::new();
    }
    let mut chunks = stop_active_block(state);
    let index = state.next_index;
    state.next_index += 1;
    state.active_block = Some(block_type);
    let content_block = match block_type {
        BlockType::Text => json!({"type": "text", "text": ""}),
        BlockType::Thinking => json!({"type": "thinking", "thinking": ""}),
        BlockType::ToolUse => json!({"type": "tool_use", "input": {}}),
        BlockType::ServerToolUse => json!({"type": "server_tool_use", "input": {}}),
    };
    chunks.push(sse(
        &json!({
            "type": "content_block_start",
            "index": index,
            "content_block": content_block
        }),
        Some("content_block_start"),
    ));
    chunks
}

fn start_anthropic_server_tool_use(state: &mut AnthropicStreamState, data: &Value) -> Vec<String> {
    let output_index = int_field(data, "output_index");
    if state.tool_call_blocks.contains_key(&output_index) {
        return Vec::new();
    }

    let item = data.get("item").unwrap_or(&Value::Null);
    let mut chunks = stop_active_block(state);
    let index = state.next_index;
    state.next_index += 1;
    state.active_block = Some(BlockType::ServerToolUse);
    state.tool_call_blocks.insert(output_index, index);
    chunks.push(sse(
        &json!({
            "type": "content_block_start",
            "index": index,
            "content_block": {
                "type": "server_tool_use",
                "id": item.get("id").cloned().unwrap_or(Value::Null),
                "name": "web_search",
                "input": {"query": web_search_query_from_action(item.get("action").unwrap_or(&Value::Null))}
            }
        }),
        Some("content_block_start"),
    ));
    chunks
}

fn stop_active_block(state: &mut AnthropicStreamState) -> Vec<String> {
    if state.active_block.is_none() {
        return Vec::new();
    }
    let index = state.next_index - 1;
    state.active_block = None;
    vec![sse(
        &json!({"type": "content_block_stop", "index": index}),
        Some("content_block_stop"),
    )]
}

fn responses_output_index(state: &mut ResponsesStreamState, block_index: i64) -> i64 {
    if let Some(output_index) = state.block_output_indexes.get(&block_index) {
        return *output_index;
    }
    let output_index = state.next_output_index;
    state.next_output_index += 1;
    state.block_output_indexes.insert(block_index, output_index);
    output_index
}

fn response_function_item(call: &FunctionCall) -> Value {
    json!({
        "type": "function_call",
        "id": call.id,
        "call_id": call.id,
        "name": call.name,
        "arguments": call.arguments
    })
}

fn chat_delta(state: &ChatStreamState, delta: &Value) -> String {
    sse(
        &json!({
            "id": state.id,
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": state.model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": Value::Null}]
        }),
        None,
    )
}

fn chat_done(state: &ChatStreamState, reason: &str) -> String {
    sse(
        &json!({
            "id": state.id,
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": state.model,
            "choices": [{"index": 0, "delta": {}, "finish_reason": reason}]
        }),
        None,
    )
}

fn anthropic_stop_reason_to_chat(stop_reason: &str) -> &'static str {
    match stop_reason {
        "tool_use" => "tool_calls",
        "max_tokens" => "length",
        _ => "stop",
    }
}

fn update_anthropic_usage(usage: &mut UsageData, payload: &Value) {
    if let Some(input_tokens) = payload.get("input_tokens").and_then(Value::as_i64) {
        usage.input_tokens = input_tokens;
    }
    if let Some(output_tokens) = payload.get("output_tokens").and_then(Value::as_i64) {
        usage.output_tokens = output_tokens;
    }
    if let Some(cache_creation_input_tokens) = payload
        .get("cache_creation_input_tokens")
        .and_then(Value::as_i64)
    {
        usage.cache_creation_input_tokens = cache_creation_input_tokens;
    }
    if let Some(cache_read_input_tokens) = payload
        .get("cache_read_input_tokens")
        .and_then(Value::as_i64)
    {
        usage.cache_read_input_tokens = cache_read_input_tokens;
    }
}

fn update_responses_usage(usage: &mut UsageData, payload: &Value) {
    if let Some(input_tokens) = payload.get("input_tokens").and_then(Value::as_i64) {
        usage.input_tokens = input_tokens;
    }
    if let Some(output_tokens) = payload.get("output_tokens").and_then(Value::as_i64) {
        usage.output_tokens = output_tokens;
    }
    if let Some(cache_read_input_tokens) = payload
        .get("input_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_i64)
    {
        usage.cache_read_input_tokens = cache_read_input_tokens;
    }
    if let Some(reasoning_output_tokens) = payload
        .get("output_tokens_details")
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_i64)
    {
        usage.reasoning_output_tokens = reasoning_output_tokens;
    }
}

fn chat_usage(usage: &UsageData) -> Value {
    json!({
        "prompt_tokens": usage.input_tokens,
        "completion_tokens": usage.output_tokens,
        "total_tokens": usage.input_tokens + usage.output_tokens,
        "prompt_tokens_details": {"cached_tokens": usage.cache_read_input_tokens},
        "completion_tokens_details": {"reasoning_tokens": usage.reasoning_output_tokens}
    })
}

fn responses_usage(usage: &UsageData) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "total_tokens": usage.input_tokens + usage.output_tokens,
        "input_tokens_details": {"cached_tokens": usage.cache_read_input_tokens},
        "output_tokens_details": {"reasoning_tokens": usage.reasoning_output_tokens}
    })
}

fn int_field(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn web_search_query_from_action(action: &Value) -> String {
    if let Some(query) = action.get("query").and_then(Value::as_str) {
        return query.to_string();
    }
    action
        .get("queries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(object) => object.is_empty(),
        Value::String(text) => text.is_empty(),
        _ => false,
    }
}

trait MapExt {
    fn is_empty(&self) -> bool;
}

impl MapExt for serde_json::Map<String, Value> {
    fn is_empty(&self) -> bool {
        serde_json::Map::is_empty(self)
    }
}
