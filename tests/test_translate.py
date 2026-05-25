from __future__ import annotations

from pengepul.translate import (
    anthropic_to_responses,
    anthropic_to_responses_request,
    chat_to_responses_request,
    openai_to_anthropic,
    responses_to_anthropic,
    responses_to_anthropic_message,
)


def test_openai_chat_to_anthropic_translates_tools_and_system() -> None:
    out = openai_to_anthropic(
        {
            "model": "sonnet",
            "messages": [
                {"role": "system", "content": "Be terse."},
                {"role": "user", "content": "weather?"},
                {
                    "role": "assistant",
                    "tool_calls": [
                        {
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": '{"city":"SF"}',
                            },
                        }
                    ],
                },
                {"role": "tool", "tool_call_id": "call_1", "content": "sunny"},
            ],
            "max_completion_tokens": 256,
            "reasoning_effort": "high",
        }
    )

    assert out["model"] == "claude-sonnet-4-6"
    assert out["max_tokens"] == 256
    assert out["system"] == [{"type": "text", "text": "Be terse."}]
    assert out["thinking"]["budget_tokens"] == 24576
    assert out["messages"][1]["content"][0]["type"] == "tool_use"
    assert out["messages"][2]["content"][0]["type"] == "tool_result"


def test_openai_chat_to_anthropic_translates_web_search_tool() -> None:
    out = openai_to_anthropic(
        {
            "model": "sonnet",
            "messages": [{"role": "user", "content": "latest docs?"}],
            "tools": [
                {
                    "type": "web_search",
                    "max_uses": 2,
                    "filters": {"allowed_domains": ["docs.anthropic.com"]},
                    "user_location": {
                        "type": "approximate",
                        "city": "Jakarta",
                        "country": "ID",
                    },
                }
            ],
            "tool_choice": {"type": "web_search"},
        }
    )

    assert out["tools"] == [
        {
            "type": "web_search_20250305",
            "name": "web_search",
            "max_uses": 2,
            "allowed_domains": ["docs.anthropic.com"],
            "user_location": {
                "type": "approximate",
                "city": "Jakarta",
                "country": "ID",
            },
        }
    ]
    assert out["tool_choice"] == {"type": "tool", "name": "web_search"}


def test_openai_image_content_translates_to_anthropic_sources() -> None:
    out = openai_to_anthropic(
        {
            "model": "sonnet",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "inspect"},
                        {
                            "type": "image_url",
                            "image_url": {"url": "https://example.com/image.png"},
                        },
                        {
                            "type": "input_image",
                            "image_url": "data:image/png;base64,aGVsbG8=",
                        },
                    ],
                }
            ],
        }
    )

    content = out["messages"][0]["content"]
    assert content[1] == {
        "type": "image",
        "source": {"type": "url", "url": "https://example.com/image.png"},
    }
    assert content[2] == {
        "type": "image",
        "source": {"type": "base64", "media_type": "image/png", "data": "aGVsbG8="},
    }


def test_chat_and_anthropic_requests_translate_to_responses_shape() -> None:
    chat = chat_to_responses_request(
        {
            "model": "gpt-5.4",
            "messages": [
                {"role": "developer", "content": "Be exact."},
                {"role": "user", "content": "hi"},
            ],
            "reasoning_effort": "medium",
        }
    )
    assert chat["instructions"] == "Be exact."
    assert chat["input"] == [{"role": "user", "content": "hi"}]
    assert chat["reasoning"] == {"effort": "medium"}

    anthropic = anthropic_to_responses_request(
        {
            "model": "claude-sonnet-4-6",
            "system": [{"type": "text", "text": "Be exact."}],
            "max_tokens": 100,
            "thinking": {"type": "enabled", "budget_tokens": 9000},
            "messages": [{"role": "user", "content": "hi"}],
        }
    )
    assert anthropic["instructions"] == "Be exact."
    assert anthropic["max_output_tokens"] == 100
    assert anthropic["reasoning"] == {"effort": "high"}


def test_responses_request_translates_web_search_to_anthropic() -> None:
    out = responses_to_anthropic(
        {
            "model": "sonnet",
            "input": "latest docs?",
            "tools": [
                {
                    "type": "web_search",
                    "filters": {"blocked_domains": ["example.com"]},
                    "user_location": {"type": "approximate", "country": "US"},
                }
            ],
            "tool_choice": "auto",
        }
    )

    assert out["tools"] == [
        {
            "type": "web_search_20250305",
            "name": "web_search",
            "blocked_domains": ["example.com"],
            "user_location": {"type": "approximate", "country": "US"},
        }
    ]
    assert out["tool_choice"] == {"type": "auto"}


def test_responses_request_translates_function_tools_to_anthropic() -> None:
    out = responses_to_anthropic(
        {
            "model": "sonnet",
            "input": "weather?",
            "tools": [
                {
                    "type": "function",
                    "name": "get_weather",
                    "description": "Get weather",
                    "parameters": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}},
                    },
                }
            ],
        }
    )

    assert out["tools"] == [
        {
            "name": "get_weather",
            "description": "Get weather",
            "input_schema": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
            },
        }
    ]


def test_parallel_tool_calls_false_without_tools_is_not_sent_to_anthropic() -> None:
    out = responses_to_anthropic(
        {
            "model": "sonnet",
            "input": "hi",
            "parallel_tool_calls": False,
        }
    )

    assert "tools" not in out
    assert "tool_choice" not in out


def test_responses_function_call_round_translates_to_anthropic_messages() -> None:
    out = responses_to_anthropic(
        {
            "model": "sonnet",
            "input": [
                {"role": "user", "content": "weather?"},
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "get_weather",
                    "arguments": '{"city":"SF"}',
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "sunny",
                },
            ],
        }
    )

    assert out["messages"] == [
        {"role": "user", "content": "weather?"},
        {
            "role": "assistant",
            "content": [
                {
                    "type": "tool_use",
                    "id": "call_1",
                    "name": "get_weather",
                    "input": {"city": "SF"},
                }
            ],
        },
        {
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "call_1",
                    "content": "sunny",
                }
            ],
        },
    ]


def test_chat_request_preserves_responses_web_search_for_codex() -> None:
    out = chat_to_responses_request(
        {
            "model": "gpt-5.4",
            "messages": [{"role": "user", "content": "latest docs?"}],
            "responses_tools": [{"type": "web_search", "search_context_size": "low"}],
            "responses_tool_choice": "auto",
        }
    )

    assert out["tools"] == [{"type": "web_search", "search_context_size": "low"}]
    assert out["tool_choice"] == "auto"


def test_anthropic_web_search_tool_translates_to_responses() -> None:
    out = anthropic_to_responses_request(
        {
            "model": "claude-sonnet-4-6",
            "messages": [{"role": "user", "content": "latest docs?"}],
            "tools": [
                {
                    "type": "web_search_20250305",
                    "name": "web_search",
                    "allowed_domains": ["docs.anthropic.com"],
                    "user_location": {"type": "approximate", "country": "US"},
                }
            ],
        }
    )

    assert out["tools"] == [
        {
            "type": "web_search",
            "filters": {"allowed_domains": ["docs.anthropic.com"]},
            "user_location": {"type": "approximate", "country": "US"},
        }
    ]


def test_anthropic_tool_choice_translates_to_responses() -> None:
    out = anthropic_to_responses_request(
        {
            "model": "claude-sonnet-4-6",
            "messages": [{"role": "user", "content": "weather?"}],
            "tools": [
                {
                    "name": "get_weather",
                    "description": "Get weather",
                    "input_schema": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}},
                    },
                }
            ],
            "tool_choice": {"type": "tool", "name": "get_weather"},
        }
    )

    assert out["tool_choice"] == {"type": "function", "name": "get_weather"}

    assert (
        anthropic_to_responses_request(
            {
                "model": "claude-sonnet-4-6",
                "messages": [{"role": "user", "content": "weather?"}],
                "tools": [{"name": "get_weather"}],
                "tool_choice": {"type": "any"},
            }
        )["tool_choice"]
        == "required"
    )
    assert (
        anthropic_to_responses_request(
            {
                "model": "claude-sonnet-4-6",
                "messages": [{"role": "user", "content": "weather?"}],
                "tools": [{"name": "get_weather"}],
                "tool_choice": {"type": "none"},
            }
        )["tool_choice"]
        == "none"
    )


def test_anthropic_web_search_response_translates_to_responses() -> None:
    out = anthropic_to_responses(
        {
            "id": "msg_1",
            "content": [
                {
                    "type": "server_tool_use",
                    "id": "srv_1",
                    "name": "web_search",
                    "input": {"query": "latest python release"},
                },
                {
                    "type": "web_search_tool_result",
                    "tool_use_id": "srv_1",
                    "content": [
                        {
                            "type": "web_search_result",
                            "url": "https://python.org",
                            "title": "Python",
                        }
                    ],
                },
                {
                    "type": "text",
                    "text": "Python was updated.",
                    "citations": [
                        {
                            "type": "web_search_result_location",
                            "url": "https://python.org",
                            "title": "Python",
                        }
                    ],
                },
            ],
            "usage": {"input_tokens": 1, "output_tokens": 2},
        },
        "claude-sonnet-4-6",
    )

    assert out["output"][0] == {
        "type": "web_search_call",
        "id": "srv_1",
        "status": "completed",
        "action": {"type": "search", "query": "latest python release"},
    }
    assert out["output"][1]["content"][0]["annotations"] == [
        {
            "type": "url_citation",
            "url": "https://python.org",
            "title": "Python",
        }
    ]


def test_anthropic_to_responses_preserves_mixed_output_order() -> None:
    out = anthropic_to_responses(
        {
            "id": "msg_1",
            "content": [
                {"type": "text", "text": "Before."},
                {
                    "type": "server_tool_use",
                    "id": "srv_1",
                    "name": "web_search",
                    "input": {"query": "latest python release"},
                },
                {"type": "text", "text": "After search."},
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "get_weather",
                    "input": {"city": "SF"},
                },
            ],
            "usage": {"input_tokens": 1, "output_tokens": 2},
        },
        "claude-sonnet-4-6",
    )

    assert [item["type"] for item in out["output"]] == [
        "message",
        "web_search_call",
        "message",
        "function_call",
    ]
    assert out["output"][0]["content"][0]["text"] == "Before."
    assert out["output"][2]["content"][0]["text"] == "After search."
    assert out["output"][3]["call_id"] == "toolu_1"


def test_responses_web_search_response_translates_to_anthropic_message() -> None:
    out = responses_to_anthropic_message(
        {
            "id": "resp_1",
            "output": [
                {
                    "type": "web_search_call",
                    "id": "ws_1",
                    "status": "completed",
                    "action": {"type": "search", "query": "latest python release"},
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "Python was updated.",
                            "annotations": [
                                {
                                    "type": "url_citation",
                                    "url": "https://python.org",
                                    "title": "Python",
                                }
                            ],
                        }
                    ],
                },
            ],
            "usage": {"input_tokens": 1, "output_tokens": 2},
        },
        "gpt-5.4",
    )

    assert out["content"][0] == {
        "type": "server_tool_use",
        "id": "ws_1",
        "name": "web_search",
        "input": {"query": "latest python release"},
    }
    assert out["content"][1]["citations"] == [
        {
            "type": "web_search_result_location",
            "url": "https://python.org",
            "title": "Python",
        }
    ]


def test_responses_web_search_queries_translate_to_anthropic_message() -> None:
    out = responses_to_anthropic_message(
        {
            "id": "resp_1",
            "output": [
                {
                    "type": "web_search_call",
                    "id": "ws_1",
                    "status": "completed",
                    "action": {
                        "type": "search",
                        "queries": ["latest python release", "python 3.14 docs"],
                    },
                }
            ],
            "usage": {"input_tokens": 1, "output_tokens": 2},
        },
        "gpt-5.4",
    )

    assert out["content"] == [
        {
            "type": "server_tool_use",
            "id": "ws_1",
            "name": "web_search",
            "input": {"query": "latest python release\npython 3.14 docs"},
        }
    ]
