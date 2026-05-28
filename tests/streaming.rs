use pengepul::streaming::{
    AnthropicStreamState, ChatStreamState, ResponsesStreamState, anthropic_sse_to_chat,
    anthropic_sse_to_responses, parse_sse_events, responses_sse_to_anthropic,
    responses_sse_to_chat, responses_sse_to_payload,
};
use serde_json::{Value, json};

fn payload(chunk: &str) -> Value {
    let data = chunk
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&data).expect("valid SSE JSON payload")
}

#[test]
fn sse_parser_handles_crlf_and_preserves_data_spacing() {
    let events = parse_sse_events(&[
        b"event: delta\r\n".as_slice(),
        b"data:  leading space\r\n\r\n".as_slice(),
        b"data: second\r\n\r\n".as_slice(),
    ])
    .expect("parse events");

    assert_eq!(
        events,
        [
            ("delta".to_string(), " leading space".to_string()),
            ("message".to_string(), "second".to_string()),
        ]
    );
}

#[test]
fn responses_stream_to_anthropic_switches_content_blocks() {
    let mut state = AnthropicStreamState::new("gpt-5");
    let mut chunks = Vec::new();
    chunks.extend(responses_sse_to_anthropic(
        "response.created",
        &json!({}),
        &mut state,
    ));
    chunks.extend(responses_sse_to_anthropic(
        "response.reasoning_text.delta",
        &json!({"delta": "think"}),
        &mut state,
    ));
    chunks.extend(responses_sse_to_anthropic(
        "response.output_text.delta",
        &json!({"delta": "answer"}),
        &mut state,
    ));
    chunks.extend(responses_sse_to_anthropic(
        "response.completed",
        &json!({"response": {"usage": {"output_tokens": 2}}}),
        &mut state,
    ));

    let payloads = chunks
        .iter()
        .map(|chunk| payload(chunk))
        .collect::<Vec<_>>();
    assert_eq!(
        payloads
            .iter()
            .map(|payload| payload["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );
    assert_eq!(payloads[1]["index"], 0);
    assert_eq!(payloads[1]["content_block"]["type"], "thinking");
    assert_eq!(payloads[4]["index"], 1);
    assert_eq!(payloads[4]["content_block"]["type"], "text");
}

#[test]
fn anthropic_stream_to_responses_streams_function_call() {
    let mut state = ResponsesStreamState::new("claude-sonnet-4-6");
    let mut chunks = Vec::new();
    chunks.extend(anthropic_sse_to_responses(
        "content_block_start",
        &json!({
            "index": 0,
            "content_block": {
                "type": "tool_use",
                "id": "toolu_1",
                "name": "get_weather",
                "input": {}
            }
        }),
        &mut state,
        "claude-sonnet-4-6",
    ));
    chunks.extend(anthropic_sse_to_responses(
        "content_block_delta",
        &json!({"index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"city\":\"SF\"}"}}),
        &mut state,
        "claude-sonnet-4-6",
    ));
    chunks.extend(anthropic_sse_to_responses(
        "content_block_stop",
        &json!({"index": 0}),
        &mut state,
        "claude-sonnet-4-6",
    ));

    let payloads = chunks
        .iter()
        .map(|chunk| payload(chunk))
        .collect::<Vec<_>>();
    assert_eq!(
        payloads,
        [
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "toolu_1",
                    "call_id": "toolu_1",
                    "name": "get_weather",
                    "arguments": ""
                }
            }),
            json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "toolu_1",
                "output_index": 0,
                "delta": "{\"city\":\"SF\"}"
            }),
            json!({
                "type": "response.function_call_arguments.done",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "toolu_1",
                    "call_id": "toolu_1",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"SF\"}"
                }
            }),
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "toolu_1",
                    "call_id": "toolu_1",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"SF\"}"
                }
            }),
        ]
    );
}

#[test]
fn anthropic_stream_to_responses_streams_server_web_search() {
    let mut state = ResponsesStreamState::new("claude-sonnet-4-6");
    let chunks = anthropic_sse_to_responses(
        "content_block_start",
        &json!({
            "index": 0,
            "content_block": {
                "type": "server_tool_use",
                "id": "srv_1",
                "name": "web_search",
                "input": {"query": "latest Rust release"}
            }
        }),
        &mut state,
        "claude-sonnet-4-6",
    );

    assert_eq!(
        payload(&chunks[0]),
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "web_search_call",
                "id": "srv_1",
                "status": "completed",
                "action": {"type": "search", "query": "latest Rust release"}
            }
        })
    );
}

#[test]
fn anthropic_stream_to_responses_uses_distinct_indexes_for_mixed_outputs() {
    let mut state = ResponsesStreamState::new("claude-sonnet-4-6");
    let mut chunks = Vec::new();
    chunks.extend(anthropic_sse_to_responses(
        "content_block_start",
        &json!({
            "index": 0,
            "content_block": {
                "type": "server_tool_use",
                "id": "srv_1",
                "name": "web_search",
                "input": {"query": "latest Rust release"}
            }
        }),
        &mut state,
        "claude-sonnet-4-6",
    ));
    chunks.extend(anthropic_sse_to_responses(
        "content_block_start",
        &json!({"index": 1, "content_block": {"type": "text", "text": ""}}),
        &mut state,
        "claude-sonnet-4-6",
    ));

    let payloads = chunks
        .iter()
        .map(|chunk| payload(chunk))
        .collect::<Vec<_>>();
    assert_eq!(payloads[0]["output_index"], 0);
    assert_eq!(payloads[1]["output_index"], 1);
    assert_eq!(payloads[2]["output_index"], 1);
}

#[test]
fn anthropic_stream_to_chat_streams_text_and_usage() {
    let mut state = ChatStreamState::new("claude-sonnet-4-6");
    let mut chunks = Vec::new();
    chunks.extend(anthropic_sse_to_chat(
        "message_start",
        &json!({"message": {"usage": {"input_tokens": 2}}}),
        &mut state,
    ));
    chunks.extend(anthropic_sse_to_chat(
        "content_block_delta",
        &json!({"index": 0, "delta": {"type": "text_delta", "text": "pong"}}),
        &mut state,
    ));
    chunks.extend(anthropic_sse_to_chat(
        "message_delta",
        &json!({"delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
        &mut state,
    ));
    chunks.extend(anthropic_sse_to_chat(
        "message_stop",
        &json!({}),
        &mut state,
    ));

    let payloads = chunks
        .iter()
        .filter(|chunk| chunk.starts_with("data: {"))
        .map(|chunk| payload(chunk))
        .collect::<Vec<_>>();
    assert_eq!(payloads[0]["choices"][0]["delta"]["role"], "assistant");
    assert_eq!(payloads[1]["choices"][0]["delta"]["content"], "pong");
    assert_eq!(payloads[2]["choices"][0]["finish_reason"], "stop");
    assert_eq!(payloads[3]["choices"], json!([]));
    assert_eq!(payloads[3]["usage"]["prompt_tokens"], 2);
    assert_eq!(payloads[3]["usage"]["completion_tokens"], 3);
    assert!(chunks.iter().any(|chunk| chunk == "data: [DONE]\n\n"));
}

#[test]
fn anthropic_stream_to_chat_streams_tool_use() {
    let mut state = ChatStreamState::new("claude-sonnet-4-6");
    let mut chunks = Vec::new();
    chunks.extend(anthropic_sse_to_chat(
        "message_start",
        &json!({}),
        &mut state,
    ));
    chunks.extend(anthropic_sse_to_chat(
        "content_block_start",
        &json!({
            "index": 0,
            "content_block": {
                "type": "tool_use",
                "id": "toolu_1",
                "name": "get_weather",
                "input": {}
            }
        }),
        &mut state,
    ));
    chunks.extend(anthropic_sse_to_chat(
        "content_block_delta",
        &json!({"index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"city\":\"SF\"}"}}),
        &mut state,
    ));
    chunks.extend(anthropic_sse_to_chat(
        "message_delta",
        &json!({"delta": {"stop_reason": "tool_use"}}),
        &mut state,
    ));

    let payloads = chunks
        .iter()
        .map(|chunk| payload(chunk))
        .collect::<Vec<_>>();
    assert_eq!(
        payloads[1]["choices"][0]["delta"]["tool_calls"],
        json!([{
            "index": 0,
            "id": "toolu_1",
            "type": "function",
            "function": {"name": "get_weather", "arguments": ""}
        }])
    );
    assert_eq!(
        payloads[2]["choices"][0]["delta"]["tool_calls"],
        json!([{"index": 0, "function": {"arguments": "{\"city\":\"SF\"}"}}])
    );
    assert_eq!(payloads[3]["choices"][0]["finish_reason"], "tool_calls");
}

#[test]
fn anthropic_stream_to_responses_streams_text_and_completion() {
    let mut state = ResponsesStreamState::new("claude-sonnet-4-6");
    let mut chunks = Vec::new();
    chunks.extend(anthropic_sse_to_responses(
        "message_start",
        &json!({"message": {"usage": {"input_tokens": 2}}}),
        &mut state,
        "claude-sonnet-4-6",
    ));
    chunks.extend(anthropic_sse_to_responses(
        "content_block_start",
        &json!({"index": 0, "content_block": {"type": "text", "text": ""}}),
        &mut state,
        "claude-sonnet-4-6",
    ));
    chunks.extend(anthropic_sse_to_responses(
        "content_block_delta",
        &json!({"index": 0, "delta": {"type": "text_delta", "text": "pong"}}),
        &mut state,
        "claude-sonnet-4-6",
    ));
    chunks.extend(anthropic_sse_to_responses(
        "message_delta",
        &json!({"usage": {"output_tokens": 3}}),
        &mut state,
        "claude-sonnet-4-6",
    ));
    chunks.extend(anthropic_sse_to_responses(
        "message_stop",
        &json!({}),
        &mut state,
        "claude-sonnet-4-6",
    ));

    let payloads = chunks
        .iter()
        .map(|chunk| payload(chunk))
        .collect::<Vec<_>>();
    assert_eq!(payloads[1]["type"], "response.output_item.added");
    assert_eq!(payloads[2]["type"], "response.content_part.added");
    assert_eq!(payloads[3]["type"], "response.output_text.delta");
    assert_eq!(payloads[3]["delta"], "pong");
    assert_eq!(payloads[4]["type"], "response.completed");
    assert_eq!(payloads[4]["response"]["usage"]["input_tokens"], 2);
    assert_eq!(payloads[4]["response"]["usage"]["output_tokens"], 3);
}

#[test]
fn responses_sse_to_payload_drains_text_tools_and_usage() {
    let payload = responses_sse_to_payload(
        &[concat!(
            "event: response.output_item.done\n",
            "data: {\"item\":{\"type\":\"web_search_call\",\"id\":\"ws_1\",\"status\":\"completed\",\"action\":{\"type\":\"search\",\"query\":\"latest docs\"}}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"delta\":\"found it\"}\n\n",
            "event: response.function_call_arguments.done\n",
            "data: {\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"SF\\\"}\"}}\n\n",
            "event: response.completed\n",
            "data: {\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"completed\",\"model\":\"gpt-5.4\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}}\n\n",
            "data: [DONE]\n\n"
        )
        .as_bytes()],
        "gpt-5.4",
    )
    .expect("drain SSE");

    assert_eq!(payload["id"], "resp_1");
    assert_eq!(payload["output_text"], "found it");
    assert_eq!(
        payload["output"],
        json!([
            {
                "type": "web_search_call",
                "id": "ws_1",
                "status": "completed",
                "action": {"type": "search", "query": "latest docs"}
            },
            {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "found it"}]
            },
            {
                "type": "function_call",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": "{\"city\":\"SF\"}"
            }
        ])
    );
    assert_eq!(
        payload["usage"],
        json!({"input_tokens": 1, "output_tokens": 2})
    );
}

#[test]
fn responses_sse_to_payload_exposes_upstream_error_without_content() {
    let payload = responses_sse_to_payload(
        &[concat!(
            "event: response.failed\n",
            "data: {\"error\":{\"message\":\"model overloaded\"}}\n\n",
            "data: [DONE]\n\n"
        )
        .as_bytes()],
        "gpt-5.4",
    )
    .expect("drain SSE");

    assert_eq!(
        payload["error"],
        json!({"message": "model overloaded", "type": "upstream_error"})
    );
}

#[test]
fn responses_stream_to_chat_streams_text_and_usage() {
    let mut state = ChatStreamState::new("gpt-5.4");
    let mut chunks = Vec::new();
    chunks.extend(responses_sse_to_chat(
        "response.created",
        &json!({}),
        &mut state,
    ));
    chunks.extend(responses_sse_to_chat(
        "response.output_text.delta",
        &json!({"delta": "ok"}),
        &mut state,
    ));
    chunks.extend(responses_sse_to_chat(
        "response.completed",
        &json!({"response": {"usage": {"input_tokens": 1, "output_tokens": 2}}}),
        &mut state,
    ));

    let payloads = chunks
        .iter()
        .filter(|chunk| chunk.starts_with("data: {"))
        .map(|chunk| payload(chunk))
        .collect::<Vec<_>>();
    assert_eq!(payloads[0]["choices"][0]["delta"]["role"], "assistant");
    assert_eq!(payloads[1]["choices"][0]["delta"]["content"], "ok");
    assert_eq!(payloads[2]["choices"][0]["finish_reason"], "stop");
    assert_eq!(payloads[3]["choices"], json!([]));
    assert_eq!(payloads[3]["usage"]["prompt_tokens"], 1);
    assert_eq!(payloads[3]["usage"]["completion_tokens"], 2);
    assert!(chunks.iter().any(|chunk| chunk == "data: [DONE]\n\n"));
}

#[test]
fn responses_stream_to_anthropic_streams_web_search_call() {
    let mut state = AnthropicStreamState::new("gpt-5.4");
    let chunks = responses_sse_to_anthropic(
        "response.output_item.added",
        &json!({
            "output_index": 0,
            "item": {
                "type": "web_search_call",
                "id": "ws_1",
                "status": "completed",
                "action": {"type": "search", "query": "latest Rust release"}
            }
        }),
        &mut state,
    );

    assert_eq!(
        payload(&chunks[0]),
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {
                "type": "server_tool_use",
                "id": "ws_1",
                "name": "web_search",
                "input": {"query": "latest Rust release"}
            }
        })
    );
}

#[test]
fn responses_stream_to_anthropic_preserves_web_search_queries() {
    let mut state = AnthropicStreamState::new("gpt-5.4");
    let chunks = responses_sse_to_anthropic(
        "response.output_item.added",
        &json!({
            "output_index": 0,
            "item": {
                "type": "web_search_call",
                "id": "ws_1",
                "status": "completed",
                "action": {
                    "type": "search",
                    "queries": ["latest Rust release", "Rust 1.95 docs"]
                }
            }
        }),
        &mut state,
    );

    assert_eq!(
        payload(&chunks[0])["content_block"]["input"],
        json!({"query": "latest Rust release\nRust 1.95 docs"})
    );
}

#[test]
fn responses_stream_to_anthropic_dedupes_web_search_done_after_added() {
    let mut state = AnthropicStreamState::new("gpt-5.4");
    let added = responses_sse_to_anthropic(
        "response.output_item.added",
        &json!({
            "output_index": 0,
            "item": {
                "type": "web_search_call",
                "id": "ws_1",
                "status": "completed",
                "action": {"type": "search", "query": "latest Rust release"}
            }
        }),
        &mut state,
    );
    let done = responses_sse_to_anthropic(
        "response.output_item.done",
        &json!({
            "output_index": 0,
            "item": {
                "type": "web_search_call",
                "id": "ws_1",
                "status": "completed",
                "action": {"type": "search", "query": "latest Rust release"}
            }
        }),
        &mut state,
    );

    assert_eq!(added.len(), 1);
    assert!(done.is_empty());
}

#[test]
fn responses_stream_to_chat_streams_function_call() {
    let mut state = ChatStreamState::new("gpt-5.4");
    let mut chunks = Vec::new();
    chunks.extend(responses_sse_to_chat(
        "response.created",
        &json!({}),
        &mut state,
    ));
    chunks.extend(responses_sse_to_chat(
        "response.output_item.added",
        &json!({
            "output_index": 0,
            "item": {
                "id": "fc_1",
                "type": "function_call",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": ""
            }
        }),
        &mut state,
    ));
    chunks.extend(responses_sse_to_chat(
        "response.function_call_arguments.delta",
        &json!({"output_index": 0, "delta": "{\"city\":\"SF\"}"}),
        &mut state,
    ));
    chunks.extend(responses_sse_to_chat(
        "response.completed",
        &json!({"response": {}}),
        &mut state,
    ));

    let payloads = chunks
        .iter()
        .filter(|chunk| chunk.starts_with("data: {"))
        .map(|chunk| payload(chunk))
        .collect::<Vec<_>>();
    assert_eq!(
        payloads[1]["choices"][0]["delta"]["tool_calls"],
        json!([{
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": {"name": "get_weather", "arguments": ""}
        }])
    );
    assert_eq!(
        payloads[2]["choices"][0]["delta"]["tool_calls"],
        json!([{"index": 0, "function": {"arguments": "{\"city\":\"SF\"}"}}])
    );
    assert_eq!(payloads[3]["choices"][0]["finish_reason"], "tool_calls");
}

#[test]
fn responses_stream_to_chat_uses_done_function_arguments_when_no_delta() {
    let mut state = ChatStreamState::new("gpt-5.4");
    let mut chunks = Vec::new();
    chunks.extend(responses_sse_to_chat(
        "response.output_item.added",
        &json!({
            "output_index": 0,
            "item": {
                "id": "fc_1",
                "type": "function_call",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": ""
            }
        }),
        &mut state,
    ));
    chunks.extend(responses_sse_to_chat(
        "response.function_call_arguments.done",
        &json!({
            "output_index": 0,
            "item": {
                "id": "fc_1",
                "type": "function_call",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": "{\"city\":\"SF\"}"
            }
        }),
        &mut state,
    ));

    let payloads = chunks
        .iter()
        .map(|chunk| payload(chunk))
        .collect::<Vec<_>>();
    assert_eq!(
        payloads[1]["choices"][0]["delta"]["tool_calls"],
        json!([{"index": 0, "function": {"arguments": "{\"city\":\"SF\"}"}}])
    );
}

#[test]
fn responses_stream_to_anthropic_uses_done_function_arguments_when_no_delta() {
    let mut state = AnthropicStreamState::new("gpt-5.4");
    let mut chunks = Vec::new();
    chunks.extend(responses_sse_to_anthropic(
        "response.output_item.added",
        &json!({
            "output_index": 0,
            "item": {
                "id": "fc_1",
                "type": "function_call",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": ""
            }
        }),
        &mut state,
    ));
    chunks.extend(responses_sse_to_anthropic(
        "response.function_call_arguments.done",
        &json!({
            "output_index": 0,
            "item": {
                "id": "fc_1",
                "type": "function_call",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": "{\"city\":\"SF\"}"
            }
        }),
        &mut state,
    ));

    let payloads = chunks
        .iter()
        .map(|chunk| payload(chunk))
        .collect::<Vec<_>>();
    assert_eq!(
        payloads[1],
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": "{\"city\":\"SF\"}"}
        })
    );
}
