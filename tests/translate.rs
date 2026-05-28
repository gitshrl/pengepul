use pengepul::translate::{
    anthropic_to_responses, anthropic_to_responses_request, chat_to_responses_request,
    openai_to_anthropic, responses_to_anthropic, responses_to_anthropic_message,
};
use serde_json::json;

#[test]
fn openai_chat_to_anthropic_translates_tools_and_system() {
    let out = openai_to_anthropic(&json!({
        "model": "sonnet",
        "messages": [
            {"role": "system", "content": "Be terse."},
            {"role": "user", "content": "weather?"},
            {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"SF\"}"}
                }]
            },
            {"role": "tool", "tool_call_id": "call_1", "content": "sunny"}
        ],
        "max_completion_tokens": 256,
        "reasoning_effort": "high"
    }));

    assert_eq!(out["model"], "claude-sonnet-4-6");
    assert_eq!(out["max_tokens"], 256);
    assert_eq!(
        out["system"],
        json!([{"type": "text", "text": "Be terse."}])
    );
    assert_eq!(out["thinking"]["budget_tokens"], 24576);
    assert_eq!(out["messages"][1]["content"][0]["type"], "tool_use");
    assert_eq!(out["messages"][2]["content"][0]["type"], "tool_result");
}

#[test]
fn openai_chat_to_anthropic_translates_web_search_tool() {
    let out = openai_to_anthropic(&json!({
        "model": "sonnet",
        "messages": [{"role": "user", "content": "latest docs?"}],
        "tools": [{
            "type": "web_search",
            "max_uses": 2,
            "filters": {"allowed_domains": ["docs.anthropic.com"]},
            "user_location": {"type": "approximate", "city": "Jakarta", "country": "ID"}
        }],
        "tool_choice": {"type": "web_search"}
    }));

    assert_eq!(
        out["tools"],
        json!([{
            "type": "web_search_20260209",
            "name": "web_search",
            "max_uses": 2,
            "allowed_domains": ["docs.anthropic.com"],
            "user_location": {"type": "approximate", "city": "Jakarta", "country": "ID"}
        }])
    );
    assert_eq!(
        out["tool_choice"],
        json!({"type": "tool", "name": "web_search"})
    );
}

#[test]
fn openai_chat_to_anthropic_preserves_explicit_web_search_version() {
    let out = openai_to_anthropic(&json!({
        "model": "sonnet",
        "messages": [{"role": "user", "content": "latest docs?"}],
        "tools": [{"type": "web_search_20250305", "name": "web_search"}]
    }));

    assert_eq!(
        out["tools"],
        json!([{"type": "web_search_20250305", "name": "web_search"}])
    );
}

#[test]
fn openai_image_content_translates_to_anthropic_sources() {
    let out = openai_to_anthropic(&json!({
        "model": "sonnet",
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "inspect"},
                {"type": "image_url", "image_url": {"url": "https://example.com/image.png"}},
                {"type": "input_image", "image_url": "data:image/png;base64,aGVsbG8="}
            ]
        }]
    }));

    assert_eq!(
        out["messages"][0]["content"][1],
        json!({"type": "image", "source": {"type": "url", "url": "https://example.com/image.png"}})
    );
    assert_eq!(
        out["messages"][0]["content"][2],
        json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aGVsbG8="}})
    );
}

#[test]
fn anthropic_images_translate_to_responses_input_images() {
    let out = anthropic_to_responses_request(&json!({
        "model": "claude-sonnet-4-6",
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "inspect"},
                {"type": "image", "source": {"type": "url", "url": "https://example.com/image.png"}},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aGVsbG8="}}
            ]
        }]
    }));

    assert_eq!(
        out["input"],
        json!([{
            "role": "user",
            "content": [
                {"type": "input_text", "text": "inspect"},
                {"type": "input_image", "image_url": "https://example.com/image.png"},
                {"type": "input_image", "image_url": "data:image/png;base64,aGVsbG8="}
            ]
        }])
    );
}

#[test]
fn chat_and_anthropic_requests_translate_to_responses_shape() {
    let chat = chat_to_responses_request(&json!({
        "model": "gpt-5.4",
        "messages": [
            {"role": "developer", "content": "Be exact."},
            {"role": "user", "content": "hi"}
        ],
        "reasoning_effort": "medium"
    }));
    assert_eq!(chat["instructions"], "Be exact.");
    assert_eq!(chat["input"], json!([{"role": "user", "content": "hi"}]));
    assert_eq!(chat["reasoning"], json!({"effort": "medium"}));

    let anthropic = anthropic_to_responses_request(&json!({
        "model": "claude-sonnet-4-6",
        "system": [{"type": "text", "text": "Be exact."}],
        "max_tokens": 100,
        "thinking": {"type": "enabled", "budget_tokens": 9000},
        "messages": [{"role": "user", "content": "hi"}]
    }));
    assert_eq!(anthropic["instructions"], "Be exact.");
    assert_eq!(anthropic["max_output_tokens"], 100);
    assert_eq!(anthropic["reasoning"], json!({"effort": "high"}));
}

#[test]
fn responses_request_translates_tools_to_anthropic() {
    let web = responses_to_anthropic(&json!({
        "model": "sonnet",
        "input": "latest docs?",
        "tools": [{
            "type": "web_search",
            "filters": {"blocked_domains": ["example.com"]},
            "user_location": {"type": "approximate", "country": "US"}
        }],
        "tool_choice": "auto"
    }));
    assert_eq!(
        web["tools"],
        json!([{
            "type": "web_search_20260209",
            "name": "web_search",
            "blocked_domains": ["example.com"],
            "user_location": {"type": "approximate", "country": "US"}
        }])
    );
    assert_eq!(web["tool_choice"], json!({"type": "auto"}));

    let function = responses_to_anthropic(&json!({
        "model": "sonnet",
        "input": "weather?",
        "tools": [{
            "type": "function",
            "name": "get_weather",
            "description": "Get weather",
            "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
        }]
    }));
    assert_eq!(
        function["tools"],
        json!([{
            "name": "get_weather",
            "description": "Get weather",
            "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}
        }])
    );
}

#[test]
fn parallel_tool_calls_false_without_tools_is_not_sent_to_anthropic() {
    let out = responses_to_anthropic(&json!({
        "model": "sonnet",
        "input": "hi",
        "parallel_tool_calls": false
    }));

    assert!(out.get("tools").is_none());
    assert!(out.get("tool_choice").is_none());
}

#[test]
fn responses_function_call_round_translates_to_anthropic_messages() {
    let out = responses_to_anthropic(&json!({
        "model": "sonnet",
        "input": [
            {"role": "user", "content": "weather?"},
            {"type": "function_call", "call_id": "call_1", "name": "get_weather", "arguments": "{\"city\":\"SF\"}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "sunny"}
        ]
    }));

    assert_eq!(
        out["messages"],
        json!([
            {"role": "user", "content": "weather?"},
            {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "call_1",
                "name": "get_weather",
                "input": {"city": "SF"}
            }]},
            {"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": "call_1",
                "content": "sunny"
            }]}
        ])
    );
}

#[test]
fn chat_request_preserves_responses_web_search_for_codex() {
    let out = chat_to_responses_request(&json!({
        "model": "gpt-5.4",
        "messages": [{"role": "user", "content": "latest docs?"}],
        "responses_tools": [{"type": "web_search", "search_context_size": "low"}],
        "responses_tool_choice": "auto"
    }));

    assert_eq!(
        out["tools"],
        json!([{"type": "web_search", "search_context_size": "low"}])
    );
    assert_eq!(out["tool_choice"], "auto");
}

#[test]
fn anthropic_web_search_tool_translates_to_responses() {
    let out = anthropic_to_responses_request(&json!({
        "model": "claude-sonnet-4-6",
        "messages": [{"role": "user", "content": "latest docs?"}],
        "tools": [{
            "type": "web_search_20250305",
            "name": "web_search",
            "allowed_domains": ["docs.anthropic.com"],
            "user_location": {"type": "approximate", "country": "US"}
        }]
    }));

    assert_eq!(
        out["tools"],
        json!([{
            "type": "web_search",
            "filters": {"allowed_domains": ["docs.anthropic.com"]},
            "user_location": {"type": "approximate", "country": "US"}
        }])
    );
}

#[test]
fn anthropic_tool_choice_translates_to_responses() {
    let out = anthropic_to_responses_request(&json!({
        "model": "claude-sonnet-4-6",
        "messages": [{"role": "user", "content": "weather?"}],
        "tools": [{
            "name": "get_weather",
            "description": "Get weather",
            "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}
        }],
        "tool_choice": {"type": "tool", "name": "get_weather"}
    }));

    assert_eq!(
        out["tool_choice"],
        json!({"type": "function", "name": "get_weather"})
    );

    let required = anthropic_to_responses_request(&json!({
        "model": "claude-sonnet-4-6",
        "messages": [{"role": "user", "content": "weather?"}],
        "tools": [{"name": "get_weather"}],
        "tool_choice": {"type": "any"}
    }));
    assert_eq!(required["tool_choice"], "required");

    let none = anthropic_to_responses_request(&json!({
        "model": "claude-sonnet-4-6",
        "messages": [{"role": "user", "content": "weather?"}],
        "tools": [{"name": "get_weather"}],
        "tool_choice": {"type": "none"}
    }));
    assert_eq!(none["tool_choice"], "none");
}

#[test]
fn anthropic_web_search_response_translates_to_responses() {
    let out = anthropic_to_responses(
        &json!({
            "id": "msg_1",
            "content": [
                {
                    "type": "server_tool_use",
                    "id": "srv_1",
                    "name": "web_search",
                    "input": {"query": "latest Rust release"}
                },
                {
                    "type": "web_search_tool_result",
                    "tool_use_id": "srv_1",
                    "content": [{
                        "type": "web_search_result",
                        "url": "https://rust-lang.org",
                        "title": "Rust"
                    }]
                },
                {
                    "type": "text",
                    "text": "Rust was updated.",
                    "citations": [{
                        "type": "web_search_result_location",
                        "url": "https://rust-lang.org",
                        "title": "Rust"
                    }]
                }
            ],
            "usage": {"input_tokens": 1, "output_tokens": 2}
        }),
        "claude-sonnet-4-6",
    );

    assert_eq!(
        out["output"][0],
        json!({
            "type": "web_search_call",
            "id": "srv_1",
            "status": "completed",
            "action": {"type": "search", "query": "latest Rust release"}
        })
    );
    assert_eq!(
        out["output"][1]["content"][0]["annotations"],
        json!([{
            "type": "url_citation",
            "url": "https://rust-lang.org",
            "title": "Rust"
        }])
    );
}

#[test]
fn anthropic_to_responses_preserves_mixed_output_order_and_output_text() {
    let out = anthropic_to_responses(
        &json!({
            "id": "msg_1",
            "content": [
                {"type": "text", "text": "Before."},
                {"type": "server_tool_use", "id": "srv_1", "name": "web_search", "input": {"query": "latest Rust release"}},
                {"type": "text", "text": "After search."},
                {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "SF"}}
            ],
            "usage": {"input_tokens": 1, "output_tokens": 2}
        }),
        "claude-sonnet-4-6",
    );

    assert_eq!(
        out["output"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["message", "web_search_call", "message", "function_call"]
    );
    assert_eq!(out["output_text"], "Before.After search.");
    assert_eq!(out["output"][3]["call_id"], "toolu_1");
}

#[test]
fn responses_web_search_response_translates_to_anthropic_message() {
    let out = responses_to_anthropic_message(
        &json!({
            "id": "resp_1",
            "output": [
                {"type": "web_search_call", "id": "ws_1", "status": "completed", "action": {"type": "search", "query": "latest Rust release"}},
                {"type": "message", "role": "assistant", "content": [{
                    "type": "output_text",
                    "text": "Rust was updated.",
                    "annotations": [{"type": "url_citation", "url": "https://rust-lang.org", "title": "Rust"}]
                }]}
            ],
            "usage": {"input_tokens": 1, "output_tokens": 2}
        }),
        "gpt-5.4",
    );

    assert_eq!(
        out["content"][0],
        json!({
            "type": "server_tool_use",
            "id": "ws_1",
            "name": "web_search",
            "input": {"query": "latest Rust release"}
        })
    );
    assert_eq!(
        out["content"][1]["citations"],
        json!([{"type": "web_search_result_location", "url": "https://rust-lang.org", "title": "Rust"}])
    );
}

#[test]
fn responses_function_call_translates_to_anthropic_tool_use_stop_reason() {
    let out = responses_to_anthropic_message(
        &json!({
            "id": "resp_1",
            "output": [{
                "type": "function_call",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": "{\"city\":\"SF\"}"
            }],
            "usage": {"input_tokens": 1, "output_tokens": 2}
        }),
        "gpt-5.4",
    );

    assert_eq!(
        out["content"],
        json!([{
            "type": "tool_use",
            "id": "call_1",
            "name": "get_weather",
            "input": {"city": "SF"}
        }])
    );
    assert_eq!(out["stop_reason"], "tool_use");
}

#[test]
fn responses_web_search_queries_translate_to_anthropic_message() {
    let out = responses_to_anthropic_message(
        &json!({
            "id": "resp_1",
            "output": [{
                "type": "web_search_call",
                "id": "ws_1",
                "status": "completed",
                "action": {
                    "type": "search",
                    "queries": ["latest Rust release", "Rust 1.95 docs"]
                }
            }],
            "usage": {"input_tokens": 1, "output_tokens": 2}
        }),
        "gpt-5.4",
    );

    assert_eq!(
        out["content"],
        json!([{
            "type": "server_tool_use",
            "id": "ws_1",
            "name": "web_search",
            "input": {"query": "latest Rust release\nRust 1.95 docs"}
        }])
    );
}
