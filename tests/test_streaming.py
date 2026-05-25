from __future__ import annotations

import asyncio

from helpers import collect_sse, sse_payload

from pengepul.streaming import (
    AnthropicStreamState,
    ChatStreamState,
    ResponsesStreamState,
    anthropic_sse_to_chat,
    anthropic_sse_to_responses,
    passthrough_sse,
    responses_sse_to_anthropic,
    responses_sse_to_chat,
)
from pengepul.translate import ResponsesDrain, update_drain_from_response_event
from pengepul.types import UsageData


class _FakeClosableSSE:
    def __init__(self, chunks: list[bytes]) -> None:
        self._chunks = chunks
        self.closed = False

    async def aiter_bytes(self):
        for chunk in self._chunks:
            yield chunk

    async def aclose(self) -> None:
        self.closed = True


def test_sse_parser_handles_crlf_and_preserves_data_spacing() -> None:
    events = asyncio.run(
        collect_sse(
            [
                b"event: delta\r\n",
                b"data:  leading space\r\n\r\n",
                b"data: second\r\n\r\n",
            ]
        )
    )

    assert events == [("delta", " leading space"), ("message", "second")]


def test_passthrough_sse_marks_anthropic_message_stop_success() -> None:
    upstream = _FakeClosableSSE(
        [
            (b'event: message_start\ndata: {"message":{"usage":{"input_tokens":3}}}\n\n'),
            b'event: message_delta\ndata: {"usage":{"output_tokens":5}}\n\n',
            b'event: message_stop\ndata: {"type":"message_stop"}\n\n',
        ]
    )
    successes: list[UsageData] = []
    failures: list[str] = []

    async def collect() -> list[bytes]:
        return [
            chunk async for chunk in passthrough_sse(upstream, successes.append, failures.append)
        ]

    chunks = asyncio.run(collect())

    assert failures == []
    assert [(usage.input_tokens, usage.output_tokens) for usage in successes] == [(3, 5)]
    assert any(b"event: message_stop" in chunk for chunk in chunks)
    assert upstream.closed is True


def test_responses_stream_to_anthropic_switches_content_blocks() -> None:
    state = AnthropicStreamState(model="gpt-5")
    chunks: list[str] = []
    chunks.extend(responses_sse_to_anthropic("response.created", {}, state))
    chunks.extend(
        responses_sse_to_anthropic("response.reasoning_text.delta", {"delta": "think"}, state)
    )
    chunks.extend(
        responses_sse_to_anthropic("response.output_text.delta", {"delta": "answer"}, state)
    )
    chunks.extend(
        responses_sse_to_anthropic(
            "response.completed",
            {"response": {"usage": {"output_tokens": 2}}},
            state,
        )
    )

    payloads = [sse_payload(chunk) for chunk in chunks]
    assert [payload["type"] for payload in payloads] == [
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
    assert payloads[1]["index"] == 0
    assert payloads[1]["content_block"]["type"] == "thinking"
    assert payloads[4]["index"] == 1
    assert payloads[4]["content_block"]["type"] == "text"


def test_anthropic_stream_to_responses_streams_server_web_search() -> None:
    state = ResponsesStreamState(model="claude-sonnet-4-6")
    chunks = anthropic_sse_to_responses(
        "content_block_start",
        {
            "index": 0,
            "content_block": {
                "type": "server_tool_use",
                "id": "srv_1",
                "name": "web_search",
                "input": {"query": "latest python release"},
            },
        },
        state,
        "claude-sonnet-4-6",
        UsageData(),
    )

    payload = sse_payload(chunks[0])
    assert payload == {
        "type": "response.output_item.added",
        "output_index": 0,
        "item": {
            "type": "web_search_call",
            "id": "srv_1",
            "status": "completed",
            "action": {"type": "search", "query": "latest python release"},
        },
    }


def test_anthropic_stream_to_responses_streams_thinking_delta() -> None:
    state = ResponsesStreamState(model="claude-sonnet-4-6")
    chunks: list[str] = []
    chunks.extend(
        anthropic_sse_to_responses(
            "content_block_start",
            {"index": 0, "content_block": {"type": "thinking", "thinking": ""}},
            state,
            "claude-sonnet-4-6",
            UsageData(),
        )
    )
    chunks.extend(
        anthropic_sse_to_responses(
            "content_block_delta",
            {"index": 0, "delta": {"type": "thinking_delta", "thinking": "think"}},
            state,
            "claude-sonnet-4-6",
            UsageData(),
        )
    )

    payloads = [sse_payload(chunk) for chunk in chunks]
    assert payloads == [
        {
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {"type": "reasoning", "summary": []},
        },
        {
            "type": "response.reasoning_text.delta",
            "output_index": 0,
            "delta": "think",
        },
    ]


def test_anthropic_stream_to_responses_streams_function_call() -> None:
    state = ResponsesStreamState(model="claude-sonnet-4-6")
    chunks: list[str] = []
    chunks.extend(
        anthropic_sse_to_responses(
            "content_block_start",
            {
                "index": 0,
                "content_block": {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "get_weather",
                    "input": {},
                },
            },
            state,
            "claude-sonnet-4-6",
            UsageData(),
        )
    )
    chunks.extend(
        anthropic_sse_to_responses(
            "content_block_delta",
            {"index": 0, "delta": {"type": "input_json_delta", "partial_json": '{"city":"SF"}'}},
            state,
            "claude-sonnet-4-6",
            UsageData(),
        )
    )
    chunks.extend(
        anthropic_sse_to_responses(
            "content_block_stop",
            {"index": 0},
            state,
            "claude-sonnet-4-6",
            UsageData(),
        )
    )

    payloads = [sse_payload(chunk) for chunk in chunks]
    assert payloads == [
        {
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "id": "toolu_1",
                "call_id": "toolu_1",
                "name": "get_weather",
                "arguments": "",
            },
        },
        {
            "type": "response.function_call_arguments.delta",
            "item_id": "toolu_1",
            "output_index": 0,
            "delta": '{"city":"SF"}',
        },
        {
            "type": "response.function_call_arguments.done",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "id": "toolu_1",
                "call_id": "toolu_1",
                "name": "get_weather",
                "arguments": '{"city":"SF"}',
            },
        },
        {
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "id": "toolu_1",
                "call_id": "toolu_1",
                "name": "get_weather",
                "arguments": '{"city":"SF"}',
            },
        },
    ]


def test_anthropic_stream_to_responses_uses_distinct_indexes_for_mixed_outputs() -> None:
    state = ResponsesStreamState(model="claude-sonnet-4-6")
    chunks: list[str] = []
    chunks.extend(
        anthropic_sse_to_responses(
            "content_block_start",
            {
                "index": 0,
                "content_block": {
                    "type": "server_tool_use",
                    "id": "srv_1",
                    "name": "web_search",
                    "input": {"query": "latest python release"},
                },
            },
            state,
            "claude-sonnet-4-6",
            UsageData(),
        )
    )
    chunks.extend(
        anthropic_sse_to_responses(
            "content_block_start",
            {"index": 2, "content_block": {"type": "text", "text": ""}},
            state,
            "claude-sonnet-4-6",
            UsageData(),
        )
    )
    chunks.extend(
        anthropic_sse_to_responses(
            "content_block_delta",
            {"index": 2, "delta": {"type": "text_delta", "text": "after"}},
            state,
            "claude-sonnet-4-6",
            UsageData(),
        )
    )

    payloads = [sse_payload(chunk) for chunk in chunks]
    assert payloads[0]["output_index"] == 0
    assert payloads[1]["output_index"] == 1
    assert payloads[2]["output_index"] == 1


def test_responses_stream_to_anthropic_streams_web_search_call() -> None:
    state = AnthropicStreamState(model="gpt-5.4")
    chunks = responses_sse_to_anthropic(
        "response.output_item.added",
        {
            "output_index": 0,
            "item": {
                "type": "web_search_call",
                "id": "ws_1",
                "status": "completed",
                "action": {"type": "search", "query": "latest python release"},
            },
        },
        state,
    )

    payload = sse_payload(chunks[0])
    assert payload == {
        "type": "content_block_start",
        "index": 0,
        "content_block": {
            "type": "server_tool_use",
            "id": "ws_1",
            "name": "web_search",
            "input": {"query": "latest python release"},
        },
    }


def test_responses_stream_to_anthropic_preserves_web_search_queries() -> None:
    state = AnthropicStreamState(model="gpt-5.4")
    chunks = responses_sse_to_anthropic(
        "response.output_item.added",
        {
            "output_index": 0,
            "item": {
                "type": "web_search_call",
                "id": "ws_1",
                "status": "completed",
                "action": {
                    "type": "search",
                    "queries": ["latest python release", "python 3.14 docs"],
                },
            },
        },
        state,
    )

    payload = sse_payload(chunks[0])
    assert payload["content_block"]["input"] == {"query": "latest python release\npython 3.14 docs"}


def test_responses_drain_accumulates_streamed_function_call_arguments() -> None:
    drain = ResponsesDrain()

    update_drain_from_response_event(
        drain,
        "response.output_item.added",
        {
            "output_index": 0,
            "item": {
                "id": "fc_1",
                "type": "function_call",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": "",
            },
        },
    )
    update_drain_from_response_event(
        drain,
        "response.function_call_arguments.delta",
        {"item_id": "fc_1", "delta": '{"city"'},
    )
    update_drain_from_response_event(
        drain,
        "response.function_call_arguments.delta",
        {"item_id": "fc_1", "delta": ':"SF"}'},
    )

    assert drain.tool_calls["fc_1"] == {
        "id": "call_1",
        "name": "get_weather",
        "args": '{"city":"SF"}',
    }


def test_responses_stream_to_chat_streams_function_call() -> None:
    state = ChatStreamState(model="gpt-5.4")
    chunks: list[str] = []
    chunks.extend(responses_sse_to_chat("response.created", {}, state))
    chunks.extend(
        responses_sse_to_chat(
            "response.output_item.added",
            {
                "output_index": 0,
                "item": {
                    "id": "fc_1",
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "get_weather",
                    "arguments": "",
                },
            },
            state,
        )
    )
    chunks.extend(
        responses_sse_to_chat(
            "response.function_call_arguments.delta",
            {"output_index": 0, "delta": '{"city":"SF"}'},
            state,
        )
    )
    chunks.extend(responses_sse_to_chat("response.completed", {"response": {}}, state))

    payloads = [sse_payload(chunk) for chunk in chunks if chunk.startswith("data: {")]
    assert payloads[1]["choices"][0]["delta"]["tool_calls"] == [
        {
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": {"name": "get_weather", "arguments": ""},
        }
    ]
    assert payloads[2]["choices"][0]["delta"]["tool_calls"] == [
        {"index": 0, "function": {"arguments": '{"city":"SF"}'}}
    ]
    assert payloads[3]["choices"][0]["finish_reason"] == "tool_calls"


def test_responses_stream_to_chat_uses_done_function_arguments_when_no_delta() -> None:
    state = ChatStreamState(model="gpt-5.4")
    chunks: list[str] = []
    chunks.extend(
        responses_sse_to_chat(
            "response.output_item.added",
            {
                "output_index": 0,
                "item": {
                    "id": "fc_1",
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "get_weather",
                    "arguments": "",
                },
            },
            state,
        )
    )
    chunks.extend(
        responses_sse_to_chat(
            "response.function_call_arguments.done",
            {
                "output_index": 0,
                "item": {
                    "id": "fc_1",
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "get_weather",
                    "arguments": '{"city":"SF"}',
                },
            },
            state,
        )
    )

    payloads = [sse_payload(chunk) for chunk in chunks]
    assert payloads[1]["choices"][0]["delta"]["tool_calls"] == [
        {"index": 0, "function": {"arguments": '{"city":"SF"}'}}
    ]


def test_responses_stream_to_anthropic_uses_done_function_arguments_when_no_delta() -> None:
    state = AnthropicStreamState(model="gpt-5.4")
    chunks: list[str] = []
    chunks.extend(
        responses_sse_to_anthropic(
            "response.output_item.added",
            {
                "output_index": 0,
                "item": {
                    "id": "fc_1",
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "get_weather",
                    "arguments": "",
                },
            },
            state,
        )
    )
    chunks.extend(
        responses_sse_to_anthropic(
            "response.function_call_arguments.done",
            {
                "output_index": 0,
                "item": {
                    "id": "fc_1",
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "get_weather",
                    "arguments": '{"city":"SF"}',
                },
            },
            state,
        )
    )

    payloads = [sse_payload(chunk) for chunk in chunks]
    assert payloads[1] == {
        "type": "content_block_delta",
        "index": 0,
        "delta": {"type": "input_json_delta", "partial_json": '{"city":"SF"}'},
    }


def test_anthropic_stream_to_chat_streams_tool_use() -> None:
    state = ChatStreamState(model="claude-sonnet-4-6")
    chunks: list[str] = []
    chunks.extend(anthropic_sse_to_chat("message_start", {}, state, UsageData()))
    chunks.extend(
        anthropic_sse_to_chat(
            "content_block_start",
            {
                "index": 0,
                "content_block": {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "get_weather",
                    "input": {},
                },
            },
            state,
            UsageData(),
        )
    )
    chunks.extend(
        anthropic_sse_to_chat(
            "content_block_delta",
            {"index": 0, "delta": {"type": "input_json_delta", "partial_json": '{"city":"SF"}'}},
            state,
            UsageData(),
        )
    )
    chunks.extend(
        anthropic_sse_to_chat(
            "message_delta",
            {"delta": {"stop_reason": "tool_use"}},
            state,
            UsageData(),
        )
    )

    payloads = [sse_payload(chunk) for chunk in chunks]
    assert payloads[1]["choices"][0]["delta"]["tool_calls"] == [
        {
            "index": 0,
            "id": "toolu_1",
            "type": "function",
            "function": {"name": "get_weather", "arguments": ""},
        }
    ]
    assert payloads[2]["choices"][0]["delta"]["tool_calls"] == [
        {"index": 0, "function": {"arguments": '{"city":"SF"}'}}
    ]
    assert payloads[3]["choices"][0]["finish_reason"] == "tool_calls"
