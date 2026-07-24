use serde_json::{Map, Value, json};

const ANTHROPIC_WEB_SEARCH_TOOL_TYPE: &str = "web_search_20250305";
const ANTHROPIC_WEB_SEARCH_TOOL_TYPES: [&str; 1] = [ANTHROPIC_WEB_SEARCH_TOOL_TYPE];

pub const MODEL_ALIASES: [(&str, &str); 3] = [
    ("opus", "claude-opus-5"),
    ("sonnet", "claude-sonnet-4-6"),
    ("haiku", "claude-haiku-4-5-20251001"),
];

#[must_use]
pub fn resolve_model(model: Option<&str>) -> String {
    let Some(model) = model else {
        return "claude-sonnet-4-6".to_string();
    };
    MODEL_ALIASES
        .iter()
        .find_map(|(alias, resolved)| (*alias == model).then_some(*resolved))
        .unwrap_or(model)
        .to_string()
}

#[must_use]
pub fn openai_to_anthropic(body: &Value) -> Value {
    let messages = value_array(body.get("messages"));
    let mut out = Map::new();
    out.insert(
        "model".to_string(),
        Value::String(resolve_model(body.get("model").and_then(Value::as_str))),
    );
    out.insert(
        "max_tokens".to_string(),
        body.get("max_completion_tokens")
            .or_else(|| body.get("max_tokens"))
            .cloned()
            .unwrap_or_else(|| json!(8192)),
    );
    copy_if_present(body, &mut out, "stream");
    copy_if_present(body, &mut out, "temperature");
    copy_if_present(body, &mut out, "top_p");
    if let Some(stop) = body.get("stop") {
        out.insert(
            "stop_sequences".to_string(),
            if stop.is_array() {
                stop.clone()
            } else {
                Value::Array(vec![stop.clone()])
            },
        );
    }

    let system = anthropic_system_from_messages(&messages);
    if !system.is_empty() {
        out.insert("system".to_string(), Value::Array(system));
    }

    let mut output_messages = Vec::new();
    for message in messages {
        let role = message.get("role").and_then(Value::as_str);
        match role {
            Some("tool") => output_messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": message.get("tool_call_id").cloned().unwrap_or(Value::Null),
                    "content": text_from_content(message.get("content").unwrap_or(&Value::Null))
                }]
            })),
            Some("assistant") if message.get("tool_calls").is_some() => {
                let mut content = Vec::new();
                let text = text_from_content(message.get("content").unwrap_or(&Value::Null));
                if !text.is_empty() {
                    content.push(json!({"type": "text", "text": text}));
                }
                for call in value_array(message.get("tool_calls")) {
                    let function = call.get("function").unwrap_or(&Value::Null);
                    let args = function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}");
                    let parsed_args =
                        serde_json::from_str::<Value>(args).unwrap_or_else(|_| Value::String(args.to_string()));
                    content.push(json!({
                        "type": "tool_use",
                        "id": call.get("id").cloned().unwrap_or(Value::Null),
                        "name": function.get("name").cloned().unwrap_or(Value::Null),
                        "input": parsed_args
                    }));
                }
                output_messages.push(json!({"role": "assistant", "content": content}));
            }
            Some(message_role @ ("user" | "assistant")) => output_messages.push(json!({
                "role": message_role,
                "content": openai_content_to_anthropic(message.get("content").unwrap_or(&Value::Null))
            })),
            _ => {}
        }
    }
    out.insert("messages".to_string(), Value::Array(output_messages));

    if let Some(thinking) =
        thinking_from_effort(body.get("reasoning_effort").and_then(Value::as_str), None)
    {
        out.insert("thinking".to_string(), thinking);
    }

    let mut tools = Vec::new();
    for tool in value_array(body.get("tools"))
        .into_iter()
        .chain(value_array(body.get("responses_tools")))
    {
        if let Some(tool) = openai_tool_to_anthropic(tool) {
            tools.push(tool);
        }
    }
    if !tools.is_empty() {
        out.insert("tools".to_string(), Value::Array(tools));
    }
    if let Some(choice) = anthropic_tool_choice(body, out.get("tools").is_some()) {
        out.insert("tool_choice".to_string(), choice);
    }

    Value::Object(out)
}

#[must_use]
pub fn responses_to_anthropic(body: &Value) -> Value {
    let input = body
        .get("input")
        .or_else(|| body.get("messages"))
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let messages = responses_input_to_openai_messages(&input);
    let mut pseudo = Map::new();
    insert_if_some(&mut pseudo, "model", body.get("model").cloned());
    pseudo.insert("messages".to_string(), Value::Array(messages));
    pseudo.insert(
        "stream".to_string(),
        body.get("stream").cloned().unwrap_or(Value::Bool(false)),
    );
    copy_if_present(body, &mut pseudo, "temperature");
    copy_if_present(body, &mut pseudo, "top_p");
    if let Some(value) = body.get("max_output_tokens") {
        pseudo.insert("max_completion_tokens".to_string(), value.clone());
    }
    copy_if_present(body, &mut pseudo, "tools");
    copy_if_present(body, &mut pseudo, "tool_choice");
    copy_if_present(body, &mut pseudo, "parallel_tool_calls");

    let mut out = openai_to_anthropic(&Value::Object(pseudo));
    if let Some(instructions) = body.get("instructions").filter(|value| !is_empty(value)) {
        out["system"] = json!([{"type": "text", "text": instructions}]);
    }
    let reasoning = body.get("reasoning").unwrap_or(&Value::Null);
    if let Some(thinking) = thinking_from_effort(
        reasoning.get("effort").and_then(Value::as_str),
        reasoning.get("summary").and_then(Value::as_str),
    ) {
        out["thinking"] = thinking;
    }
    out
}

#[must_use]
pub fn anthropic_to_responses(payload: &Value, model: &str) -> Value {
    let mut output = Vec::new();
    let mut text = String::new();
    let mut output_text_parts = Vec::new();
    let mut annotations = Vec::new();

    for block in value_array(payload.get("content")) {
        match block.get("type").and_then(Value::as_str) {
            Some("thinking") => {
                flush_response_text(
                    &mut output,
                    &mut output_text_parts,
                    &mut text,
                    &mut annotations,
                );
                output.push(json!({
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": block.get("thinking").and_then(Value::as_str).unwrap_or("")}]
                }));
            }
            Some("text") => {
                text.push_str(block.get("text").and_then(Value::as_str).unwrap_or(""));
                annotations.extend(anthropic_citations_to_openai(
                    block.get("citations").and_then(Value::as_array),
                ));
            }
            Some("tool_use") => {
                flush_response_text(
                    &mut output,
                    &mut output_text_parts,
                    &mut text,
                    &mut annotations,
                );
                output.push(json!({
                    "type": "function_call",
                    "call_id": block.get("id").cloned().unwrap_or(Value::Null),
                    "name": block.get("name").cloned().unwrap_or(Value::Null),
                    "arguments": serde_json::to_string(block.get("input").unwrap_or(&json!({}))).unwrap_or_else(|_| "{}".to_string())
                }));
            }
            Some("server_tool_use")
                if block.get("name").and_then(Value::as_str) == Some("web_search") =>
            {
                flush_response_text(
                    &mut output,
                    &mut output_text_parts,
                    &mut text,
                    &mut annotations,
                );
                output.push(json!({
                    "type": "web_search_call",
                    "id": block.get("id").cloned().unwrap_or(Value::Null),
                    "status": "completed",
                    "action": {
                        "type": "search",
                        "query": block.get("input").and_then(|input| input.get("query")).and_then(Value::as_str).unwrap_or("")
                    }
                }));
            }
            _ => {}
        }
    }
    flush_response_text(
        &mut output,
        &mut output_text_parts,
        &mut text,
        &mut annotations,
    );

    let usage = payload.get("usage").unwrap_or(&Value::Null);
    let input_tokens = int_field(usage, "input_tokens");
    let output_tokens = int_field(usage, "output_tokens");
    json!({
        "id": payload.get("id").cloned().unwrap_or_else(|| Value::String(format!("resp_{}", uuid::Uuid::new_v4().simple()))),
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "status": if payload.get("stop_reason").and_then(Value::as_str) == Some("max_tokens") { "incomplete" } else { "completed" },
        "model": model,
        "output": output,
        "output_text": output_text_parts.join(""),
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
            "input_tokens_details": {"cached_tokens": int_field(usage, "cache_read_input_tokens")},
            "output_tokens_details": {"reasoning_tokens": 0}
        }
    })
}

#[must_use]
pub fn anthropic_to_openai(payload: &Value, model: &str) -> Value {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();
    for block in value_array(payload.get("content")) {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                text_parts.push(
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                );
            }
            Some("tool_use") => {
                tool_calls.push(json!({
                    "id": block.get("id").cloned().unwrap_or(Value::Null),
                    "type": "function",
                    "function": {
                        "name": block.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": serde_json::to_string(block.get("input").unwrap_or(&json!({}))).unwrap_or_else(|_| "{}".to_string())
                    }
                }));
            }
            _ => {}
        }
    }

    let mut message = json!({"role": "assistant", "content": text_parts.join("")});
    let has_tool_calls = !tool_calls.is_empty();
    if has_tool_calls {
        message["tool_calls"] = Value::Array(tool_calls);
    }

    json!({
        "id": payload.get("id").cloned().unwrap_or_else(|| Value::String(format!("chatcmpl_{}", uuid::Uuid::new_v4().simple()))),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason(payload.get("stop_reason").and_then(Value::as_str), has_tool_calls)
        }],
        "usage": usage_to_openai(payload.get("usage").unwrap_or(&Value::Null))
    })
}

#[must_use]
pub fn chat_to_responses_request(body: &Value) -> Value {
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    for message in value_array(body.get("messages")) {
        let role = message.get("role").and_then(Value::as_str);
        match role {
            Some("system" | "developer") => {
                let text = text_from_content(message.get("content").unwrap_or(&Value::Null));
                if !text.is_empty() {
                    instructions.push(text);
                }
            }
            Some("tool") => input.push(json!({
                "type": "function_call_output",
                "call_id": message.get("tool_call_id").cloned().unwrap_or(Value::Null),
                "output": text_from_content(message.get("content").unwrap_or(&Value::Null))
            })),
            Some("assistant") if message.get("tool_calls").is_some() => {
                let text = text_from_content(message.get("content").unwrap_or(&Value::Null));
                if !text.is_empty() {
                    input.push(json!({"role": "assistant", "content": text}));
                }
                for call in value_array(message.get("tool_calls")) {
                    let function = call.get("function").unwrap_or(&Value::Null);
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.get("id").cloned().unwrap_or(Value::Null),
                        "name": function.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": function.get("arguments").cloned().unwrap_or_else(|| json!("{}"))
                    }));
                }
            }
            Some(message_role @ ("user" | "assistant")) => input.push(json!({
                "role": message_role,
                "content": chat_content_to_responses(message.get("content"))
            })),
            _ => {}
        }
    }

    let mut out = Map::new();
    insert_if_some(&mut out, "model", body.get("model").cloned());
    out.insert("input".to_string(), Value::Array(input));
    if !instructions.is_empty() {
        out.insert(
            "instructions".to_string(),
            Value::String(instructions.join("\n\n")),
        );
    }
    copy_if_present(body, &mut out, "stream");
    copy_if_present(body, &mut out, "temperature");
    copy_if_present(body, &mut out, "top_p");
    if let Some(tokens) = body
        .get("max_completion_tokens")
        .or_else(|| body.get("max_tokens"))
    {
        out.insert("max_output_tokens".to_string(), tokens.clone());
    }
    if let Some(effort) = body.get("reasoning_effort") {
        out.insert("reasoning".to_string(), json!({"effort": effort}));
    }
    let mut tools = Vec::new();
    for tool in value_array(body.get("tools")) {
        if let Some(tool) = responses_tool_from_chat_tool(tool) {
            tools.push(tool);
        }
    }
    for tool in value_array(body.get("responses_tools")) {
        tools.push(tool.clone());
    }
    if !tools.is_empty() {
        out.insert("tools".to_string(), Value::Array(tools));
    }
    if let Some(choice) = body.get("responses_tool_choice") {
        out.insert("tool_choice".to_string(), choice.clone());
    } else if let Some(choice) = body.get("tool_choice") {
        out.insert(
            "tool_choice".to_string(),
            responses_tool_choice_from_chat(choice),
        );
    }
    copy_if_present(body, &mut out, "parallel_tool_calls");
    Value::Object(out)
}

#[must_use]
pub fn anthropic_to_responses_request(body: &Value) -> Value {
    let mut out = Map::new();
    insert_if_some(&mut out, "model", body.get("model").cloned());
    copy_if_present(body, &mut out, "stream");
    if let Some(max_tokens) = body.get("max_tokens") {
        out.insert("max_output_tokens".to_string(), max_tokens.clone());
    }
    copy_if_present(body, &mut out, "temperature");
    copy_if_present(body, &mut out, "top_p");
    if let Some(system) = body.get("system") {
        let instructions = if let Some(system) = system.as_str() {
            system.to_string()
        } else {
            value_array(Some(system))
                .into_iter()
                .map(text_from_content)
                .collect::<Vec<_>>()
                .join("\n\n")
        };
        if !instructions.is_empty() {
            out.insert("instructions".to_string(), Value::String(instructions));
        }
    }
    let thinking = body.get("thinking").unwrap_or(&Value::Null);
    if thinking.get("type").and_then(Value::as_str) == Some("enabled") {
        out.insert(
            "reasoning".to_string(),
            json!({"effort": effort_from_budget(thinking.get("budget_tokens").and_then(Value::as_i64))}),
        );
    }

    let mut input = Vec::new();
    for message in value_array(body.get("messages")) {
        let role = message.get("role").cloned().unwrap_or(Value::Null);
        let content = message
            .get("content")
            .cloned()
            .unwrap_or(Value::String(String::new()));
        if let Some(blocks) = content.as_array() {
            let mut parts = Vec::new();
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => parts.push(json!({"type": "input_text", "text": block.get("text").cloned().unwrap_or_else(|| json!(""))})),
                    Some("image") => {
                        if let Some(image) = anthropic_image_to_responses(block) {
                            parts.push(image);
                        }
                    }
                    Some("tool_use") => {
                        flush_input_parts(&mut input, &role, &mut parts);
                        input.push(json!({
                            "type": "function_call",
                            "call_id": block.get("id").cloned().unwrap_or(Value::Null),
                            "name": block.get("name").cloned().unwrap_or(Value::Null),
                            "arguments": serde_json::to_string(block.get("input").unwrap_or(&json!({}))).unwrap_or_else(|_| "{}".to_string())
                        }));
                    }
                    Some("tool_result") => {
                        flush_input_parts(&mut input, &role, &mut parts);
                        input.push(json!({
                            "type": "function_call_output",
                            "call_id": block.get("tool_use_id").cloned().unwrap_or(Value::Null),
                            "output": text_from_content(block.get("content").unwrap_or(&Value::Null))
                        }));
                    }
                    _ => {}
                }
            }
            flush_input_parts(&mut input, &role, &mut parts);
        } else {
            input.push(json!({"role": role, "content": content}));
        }
    }
    out.insert("input".to_string(), Value::Array(input));

    let tools = value_array(body.get("tools"))
        .into_iter()
        .map(anthropic_tool_to_responses)
        .collect::<Vec<_>>();
    if !tools.is_empty() {
        out.insert("tools".to_string(), Value::Array(tools));
    }
    if let Some(choice) = body.get("tool_choice") {
        out.insert(
            "tool_choice".to_string(),
            responses_tool_choice_from_anthropic(choice),
        );
    }
    Value::Object(out)
}

#[must_use]
pub fn responses_to_chat_completion(payload: &Value, model: &str) -> Value {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();

    for item in value_array(payload.get("output")) {
        match item.get("type").and_then(Value::as_str) {
            Some("reasoning") => {
                for summary in value_array(item.get("summary")) {
                    reasoning.push_str(summary.get("text").and_then(Value::as_str).unwrap_or(""));
                }
            }
            Some("message") => {
                for content in value_array(item.get("content")) {
                    if matches!(
                        content.get("type").and_then(Value::as_str),
                        Some("output_text" | "text")
                    ) {
                        text.push_str(content.get("text").and_then(Value::as_str).unwrap_or(""));
                    }
                }
            }
            Some("function_call") => {
                tool_calls.push(json!({
                    "id": item.get("call_id").cloned().unwrap_or(Value::Null),
                    "type": "function",
                    "function": {
                        "name": item.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": item.get("arguments").cloned().unwrap_or_else(|| json!("{}"))
                    }
                }));
            }
            _ => {}
        }
    }

    let mut message = json!({"role": "assistant", "content": text});
    if !reasoning.is_empty() {
        message["reasoning_content"] = Value::String(reasoning);
    }
    let has_tool_calls = !tool_calls.is_empty();
    if has_tool_calls {
        message["tool_calls"] = Value::Array(tool_calls);
    }

    let usage = payload.get("usage").unwrap_or(&Value::Null);
    let input_tokens = int_field(usage, "input_tokens");
    let output_tokens = int_field(usage, "output_tokens");
    json!({
        "id": format!("chatcmpl_{}", uuid::Uuid::new_v4().simple()),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": if has_tool_calls {
                "tool_calls"
            } else if payload.get("status").and_then(Value::as_str) == Some("incomplete") {
                "length"
            } else {
                "stop"
            }
        }],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
            "prompt_tokens_details": usage.get("input_tokens_details").cloned().unwrap_or_else(|| json!({"cached_tokens": 0})),
            "completion_tokens_details": usage.get("output_tokens_details").cloned().unwrap_or_else(|| json!({"reasoning_tokens": 0}))
        }
    })
}

#[must_use]
pub fn responses_to_anthropic_message(payload: &Value, model: &str) -> Value {
    let mut content = Vec::new();
    let mut has_tool_use = false;
    for item in value_array(payload.get("output")) {
        match item.get("type").and_then(Value::as_str) {
            Some("reasoning") => {
                let text = value_array(item.get("summary"))
                    .iter()
                    .filter_map(|summary| summary.get("text").and_then(Value::as_str))
                    .collect::<String>();
                if !text.is_empty() {
                    content.push(json!({"type": "thinking", "thinking": text}));
                }
            }
            Some("message") => {
                let mut text = String::new();
                let mut citations = Vec::new();
                for block in value_array(item.get("content")) {
                    if matches!(
                        block.get("type").and_then(Value::as_str),
                        Some("output_text" | "text")
                    ) {
                        text.push_str(block.get("text").and_then(Value::as_str).unwrap_or(""));
                        citations.extend(openai_annotations_to_anthropic(
                            block.get("annotations").and_then(Value::as_array),
                        ));
                    }
                }
                if !text.is_empty() {
                    let mut text_block = json!({"type": "text", "text": text});
                    if !citations.is_empty() {
                        text_block["citations"] = Value::Array(citations);
                    }
                    content.push(text_block);
                }
            }
            Some("function_call") => {
                has_tool_use = true;
                let args = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                let parsed = serde_json::from_str::<Value>(args)
                    .unwrap_or_else(|_| Value::String(args.to_string()));
                content.push(json!({
                    "type": "tool_use",
                    "id": item.get("call_id").cloned().unwrap_or(Value::Null),
                    "name": item.get("name").cloned().unwrap_or(Value::Null),
                    "input": parsed
                }));
            }
            Some("web_search_call") => {
                content.push(json!({
                    "type": "server_tool_use",
                    "id": item.get("id").cloned().unwrap_or(Value::Null),
                    "name": "web_search",
                    "input": {"query": web_search_query_from_action(item.get("action").unwrap_or(&Value::Null))}
                }));
            }
            _ => {}
        }
    }
    let usage = payload.get("usage").unwrap_or(&Value::Null);
    json!({
        "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": if payload.get("status").and_then(Value::as_str) == Some("incomplete") {
            "max_tokens"
        } else if has_tool_use {
            "tool_use"
        } else {
            "end_turn"
        },
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": int_field(usage, "input_tokens"),
            "output_tokens": int_field(usage, "output_tokens")
        }
    })
}

fn thinking_from_effort(effort: Option<&str>, summary: Option<&str>) -> Option<Value> {
    let effort = effort?;
    let budget = match effort {
        "low" => 4_096,
        "high" => 24_576,
        _ => 8_192,
    };
    let mut thinking = json!({"type": "enabled", "budget_tokens": budget});
    if summary.is_some() {
        thinking["display"] = json!("summarized");
    }
    Some(thinking)
}

fn effort_from_budget(budget: Option<i64>) -> &'static str {
    let budget = budget.unwrap_or_default();
    if budget > 8192 {
        "high"
    } else if budget > 0 && budget <= 4096 {
        "low"
    } else {
        "medium"
    }
}

fn text_from_content(content: &Value) -> String {
    match content {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Array(items) => items.iter().map(text_from_content).collect(),
        Value::Object(object) => {
            if let Some(text) = object.get("text").and_then(Value::as_str) {
                text.to_string()
            } else if matches!(
                object.get("type").and_then(Value::as_str),
                Some("input_text" | "output_text")
            ) {
                object
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            } else if object.get("type").and_then(Value::as_str) == Some("tool_result") {
                object
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            } else {
                String::new()
            }
        }
        other => other.to_string(),
    }
}

fn openai_content_to_anthropic(content: &Value) -> Value {
    match content {
        Value::String(_) | Value::Null => Value::String(text_from_content(content)),
        Value::Array(items) => {
            let mut blocks = Vec::new();
            for item in items {
                if let Some(text) = item.as_str() {
                    blocks.push(json!({"type": "text", "text": text}));
                    continue;
                }
                let Some(item_type) = item.get("type").and_then(Value::as_str) else {
                    blocks.push(json!({"type": "text", "text": item.to_string()}));
                    continue;
                };
                match item_type {
                    "text" | "input_text" | "output_text" => {
                        blocks.push(json!({"type": "text", "text": item.get("text").cloned().unwrap_or_else(|| json!(""))}));
                    }
                    "image_url" | "input_image" => {
                        if let Some(image) =
                            image_url_to_anthropic(&image_url_value(item.get("image_url")))
                        {
                            blocks.push(image);
                        }
                    }
                    _ => {}
                }
            }
            if blocks.is_empty() {
                Value::String(String::new())
            } else {
                Value::Array(blocks)
            }
        }
        other => Value::String(other.to_string()),
    }
}

/// Convert `OpenAI` Chat content parts to their Responses equivalents.
///
/// Chat says `text`/`image_url` with a nested `{url}`; Responses wants
/// `input_text`/`input_image` with a flat url string. A plain string passes
/// through, since both APIs accept one.
fn chat_content_to_responses(content: Option<&Value>) -> Value {
    let Some(parts) = content.and_then(Value::as_array) else {
        return content.cloned().unwrap_or_else(|| json!(""));
    };
    let converted: Vec<Value> = parts
        .iter()
        .map(|part| match part.get("type").and_then(Value::as_str) {
            Some("image_url" | "input_image") => json!({
                "type": "input_image",
                "image_url": image_url_value(part.get("image_url"))
            }),
            Some("text" | "input_text" | "output_text") => json!({
                "type": "input_text",
                "text": part.get("text").cloned().unwrap_or_else(|| json!(""))
            }),
            _ => part.clone(),
        })
        .collect();
    Value::Array(converted)
}

fn image_url_value(value: Option<&Value>) -> String {
    match value {
        Some(Value::Object(object)) => object
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        Some(Value::String(value)) => value.clone(),
        _ => String::new(),
    }
}

fn image_url_to_anthropic(url: &str) -> Option<Value> {
    if url.is_empty() {
        return None;
    }
    if let Some((media_type, data)) = url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))
    {
        return Some(json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data}
        }));
    }
    Some(json!({"type": "image", "source": {"type": "url", "url": url}}))
}

fn anthropic_image_to_responses(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    match source.get("type").and_then(Value::as_str)? {
        "url" => Some(json!({"type": "input_image", "image_url": source.get("url")?.as_str()?})),
        "base64" => Some(json!({
            "type": "input_image",
            "image_url": format!(
                "data:{};base64,{}",
                source.get("media_type").and_then(Value::as_str).unwrap_or("image/png"),
                source.get("data")?.as_str()?
            )
        })),
        _ => None,
    }
}

fn anthropic_system_from_messages(messages: &[&Value]) -> Vec<Value> {
    messages
        .iter()
        .filter(|message| {
            matches!(
                message.get("role").and_then(Value::as_str),
                Some("system" | "developer")
            )
        })
        .filter_map(|message| {
            let text = text_from_content(message.get("content").unwrap_or(&Value::Null));
            (!text.is_empty()).then(|| json!({"type": "text", "text": text}))
        })
        .collect()
}

fn openai_tool_to_anthropic(tool: &Value) -> Option<Value> {
    let tool_type = tool.get("type").and_then(Value::as_str);
    if matches!(tool_type, Some("web_search" | "web_search_preview"))
        || tool_type.is_some_and(|value| ANTHROPIC_WEB_SEARCH_TOOL_TYPES.contains(&value))
    {
        return Some(anthropic_web_search_tool(tool));
    }
    if tool_type == Some("function") && tool.get("name").is_some() {
        return Some(json!({
            "name": tool.get("name").cloned().unwrap_or(Value::Null),
            "description": tool.get("description").cloned().unwrap_or_else(|| json!("")),
            "input_schema": tool.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"}))
        }));
    }
    let function = tool.get("function")?;
    Some(json!({
        "name": function.get("name").cloned().unwrap_or(Value::Null),
        "description": function.get("description").cloned().unwrap_or_else(|| json!("")),
        "input_schema": function.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"}))
    }))
}

fn anthropic_web_search_tool(tool: &Value) -> Value {
    let tool_type = tool
        .get("type")
        .and_then(Value::as_str)
        .filter(|value| ANTHROPIC_WEB_SEARCH_TOOL_TYPES.contains(value))
        .unwrap_or(ANTHROPIC_WEB_SEARCH_TOOL_TYPE);
    let mut out = json!({
        "type": tool_type,
        "name": tool.get("name").and_then(Value::as_str).unwrap_or("web_search")
    });
    copy_value_field(tool, &mut out, "max_uses");
    let filters = tool.get("filters").unwrap_or(&Value::Null);
    if let Some(value) = tool
        .get("allowed_domains")
        .or_else(|| filters.get("allowed_domains"))
    {
        out["allowed_domains"] = value.clone();
    }
    if let Some(value) = tool
        .get("blocked_domains")
        .or_else(|| filters.get("blocked_domains"))
    {
        out["blocked_domains"] = value.clone();
    }
    copy_value_field(tool, &mut out, "user_location");
    out
}

fn responses_tool_from_chat_tool(tool: &Value) -> Option<Value> {
    let tool_type = tool.get("type").and_then(Value::as_str);
    if tool_type.is_some_and(|tool_type| tool_type != "function") {
        return Some(tool.clone());
    }
    let function = tool.get("function")?;
    function.get("name")?;
    let mut out = json!({
        "type": "function",
        "name": function.get("name").cloned().unwrap_or(Value::Null),
        "description": function.get("description").cloned().unwrap_or_else(|| json!("")),
        "parameters": function.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"}))
    });
    copy_value_field(function, &mut out, "strict");
    Some(out)
}

fn anthropic_tool_to_responses(tool: &Value) -> Value {
    if tool
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|value| ANTHROPIC_WEB_SEARCH_TOOL_TYPES.contains(&value))
    {
        let mut out = json!({"type": "web_search"});
        let mut filters = Map::new();
        if let Some(value) = tool.get("allowed_domains") {
            filters.insert("allowed_domains".to_string(), value.clone());
        }
        if let Some(value) = tool.get("blocked_domains") {
            filters.insert("blocked_domains".to_string(), value.clone());
        }
        if !filters.is_empty() {
            out["filters"] = Value::Object(filters);
        }
        copy_value_field(tool, &mut out, "user_location");
        return out;
    }
    json!({
        "type": "function",
        "name": tool.get("name").cloned().unwrap_or(Value::Null),
        "description": tool.get("description").cloned().unwrap_or_else(|| json!("")),
        "parameters": tool.get("input_schema").cloned().unwrap_or_else(|| json!({"type": "object"}))
    })
}

fn anthropic_tool_choice(body: &Value, has_tools: bool) -> Option<Value> {
    if !has_tools {
        return None;
    }
    if body.get("tool_choice").is_none()
        && body.get("parallel_tool_calls").and_then(Value::as_bool) != Some(false)
    {
        return None;
    }
    let default_choice = json!("auto");
    let choice = body.get("tool_choice").unwrap_or(&default_choice);
    let mut out = match choice {
        Value::String(value) if value == "auto" => json!({"type": "auto"}),
        Value::String(value) if value == "required" => json!({"type": "any"}),
        Value::String(value) if value == "none" => json!({"type": "none"}),
        Value::Object(_) => match choice.get("type").and_then(Value::as_str) {
            Some("web_search" | "web_search_preview") => {
                json!({"type": "tool", "name": "web_search"})
            }
            Some("function") => json!({
                "type": "tool",
                "name": choice.get("name")
                    .or_else(|| choice.get("function").and_then(|function| function.get("name")))
                    .cloned()
                    .unwrap_or(Value::Null)
            }),
            Some("auto" | "any" | "none" | "tool") => choice.clone(),
            _ => json!({"type": "auto"}),
        },
        _ => json!({"type": "auto"}),
    };
    if body.get("parallel_tool_calls").and_then(Value::as_bool) == Some(false) {
        out["disable_parallel_tool_use"] = Value::Bool(true);
    }
    Some(out)
}

fn responses_tool_choice_from_chat(choice: &Value) -> Value {
    if choice.get("type").and_then(Value::as_str) == Some("function") {
        return json!({
            "type": "function",
            "name": choice.get("name")
                .or_else(|| choice.get("function").and_then(|function| function.get("name")))
                .cloned()
                .unwrap_or(Value::Null)
        });
    }
    choice.clone()
}

fn responses_tool_choice_from_anthropic(choice: &Value) -> Value {
    match choice.get("type").and_then(Value::as_str) {
        Some("auto") => json!("auto"),
        Some("any") => json!("required"),
        Some("none") => json!("none"),
        Some("tool") if choice.get("name").and_then(Value::as_str) == Some("web_search") => {
            json!({"type": "web_search"})
        }
        Some("tool") => {
            json!({"type": "function", "name": choice.get("name").cloned().unwrap_or(Value::Null)})
        }
        _ => choice.clone(),
    }
}

fn responses_input_to_openai_messages(input_items: &Value) -> Vec<Value> {
    if let Some(input) = input_items.as_str() {
        return vec![json!({"role": "user", "content": input})];
    }
    let mut messages = Vec::new();
    for item in value_array(Some(input_items)) {
        if item.get("role").is_some() {
            messages.push(item.clone());
            continue;
        }
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => messages.push(json!({
                "role": "assistant",
                "tool_calls": [{
                    "id": item.get("call_id").cloned().unwrap_or(Value::Null),
                    "type": "function",
                    "function": {
                        "name": item.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": item.get("arguments").cloned().unwrap_or_else(|| json!("{}"))
                    }
                }]
            })),
            Some("function_call_output") => messages.push(json!({
                "role": "tool",
                "tool_call_id": item.get("call_id").cloned().unwrap_or(Value::Null),
                "content": item.get("output").cloned().unwrap_or_else(|| json!(""))
            })),
            // reasoning, web_search_call and friends have no chat equivalent.
            // Serializing them into a user turn attributes machine JSON to the
            // user and inflates the prompt, so they are dropped.
            _ => {}
        }
    }
    messages
}

fn anthropic_citations_to_openai(citations: Option<&Vec<Value>>) -> Vec<Value> {
    citations
        .into_iter()
        .flatten()
        .filter(|citation| {
            citation.get("type").and_then(Value::as_str) == Some("web_search_result_location")
        })
        .map(|citation| {
            let mut out = json!({
                "type": "url_citation",
                "url": citation.get("url").cloned().unwrap_or(Value::Null),
                "title": citation.get("title").cloned().unwrap_or(Value::Null)
            });
            copy_value_field(citation, &mut out, "start_index");
            copy_value_field(citation, &mut out, "end_index");
            out
        })
        .collect()
}

fn openai_annotations_to_anthropic(annotations: Option<&Vec<Value>>) -> Vec<Value> {
    annotations
        .into_iter()
        .flatten()
        .filter(|annotation| annotation.get("type").and_then(Value::as_str) == Some("url_citation"))
        .map(|annotation| {
            let mut out = json!({
                "type": "web_search_result_location",
                "url": annotation.get("url").cloned().unwrap_or(Value::Null),
                "title": annotation.get("title").cloned().unwrap_or(Value::Null)
            });
            copy_value_field(annotation, &mut out, "start_index");
            copy_value_field(annotation, &mut out, "end_index");
            out
        })
        .collect()
}

fn web_search_query_from_action(action: &Value) -> String {
    if let Some(query) = action.get("query").and_then(Value::as_str) {
        return query.to_string();
    }
    value_array(action.get("queries"))
        .into_iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n")
}

fn finish_reason(stop_reason: Option<&str>, has_tool_calls: bool) -> &'static str {
    if has_tool_calls || stop_reason == Some("tool_use") {
        "tool_calls"
    } else if stop_reason == Some("max_tokens") {
        "length"
    } else {
        "stop"
    }
}

fn usage_to_openai(usage: &Value) -> Value {
    let prompt = int_field(usage, "input_tokens");
    let completion = int_field(usage, "output_tokens");
    json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
        "prompt_tokens_details": {"cached_tokens": int_field(usage, "cache_read_input_tokens")},
        "completion_tokens_details": {
            "reasoning_tokens": usage
                .get("output_tokens_details")
                .map_or(0, |details| int_field(details, "reasoning_tokens"))
        }
    })
}

fn flush_response_text(
    output: &mut Vec<Value>,
    output_text_parts: &mut Vec<String>,
    text: &mut String,
    annotations: &mut Vec<Value>,
) {
    if text.is_empty() {
        return;
    }
    output_text_parts.push(text.clone());
    let mut content = json!({"type": "output_text", "text": text});
    if !annotations.is_empty() {
        content["annotations"] = Value::Array(std::mem::take(annotations));
    }
    output.push(json!({"type": "message", "role": "assistant", "content": [content]}));
    text.clear();
}

fn flush_input_parts(input: &mut Vec<Value>, role: &Value, parts: &mut Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    let content = if parts
        .iter()
        .all(|part| part.get("type").and_then(Value::as_str) == Some("input_text"))
    {
        Value::String(
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect(),
        )
    } else {
        Value::Array(std::mem::take(parts))
    };
    input.push(json!({"role": role, "content": content}));
    parts.clear();
}

fn copy_if_present(source: &Value, target: &mut Map<String, Value>, key: &str) {
    if let Some(value) = source.get(key) {
        target.insert(key.to_string(), value.clone());
    }
}

fn insert_if_some(target: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        target.insert(key.to_string(), value);
    }
}

fn copy_value_field(source: &Value, target: &mut Value, key: &str) {
    if let Some(value) = source.get(key) {
        target[key] = value.clone();
    }
}

fn value_array(value: Option<&Value>) -> Vec<&Value> {
    value
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |items| items.iter().collect())
}

fn int_field(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn is_empty(value: &Value) -> bool {
    matches!(value, Value::Null) || value.as_str().is_some_and(str::is_empty)
}
