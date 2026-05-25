from __future__ import annotations

import codecs
import json
import time
import uuid
from collections.abc import AsyncIterator, Callable
from dataclasses import dataclass, field
from typing import Any

from .translate import (
    ResponsesDrain,
    update_drain_from_response_event,
    usage_from_responses,
    web_search_query_from_action,
)
from .types import UsageData
from .upstream import UpstreamResponse


def sse(data: Any, event: str | None = None) -> str:
    payload = data if isinstance(data, str) else json.dumps(data, separators=(",", ":"))
    lines = []
    if event:
        lines.append(f"event: {event}")
    for line in payload.splitlines() or [""]:
        lines.append(f"data: {line}")
    return "\n".join(lines) + "\n\n"


async def iter_sse_events(upstream: UpstreamResponse) -> AsyncIterator[tuple[str, str]]:
    decoder = codecs.getincrementaldecoder("utf-8")()
    buffer = ""
    async for chunk in upstream.aiter_bytes():
        buffer += decoder.decode(chunk)
        while separator := _find_sse_separator(buffer):
            index, length = separator
            raw, buffer = buffer[:index], buffer[index + length :]
            parsed = _parse_event(raw)
            if parsed:
                yield parsed
    buffer += decoder.decode(b"", final=True)
    if buffer.strip():
        parsed = _parse_event(buffer)
        if parsed:
            yield parsed


def _find_sse_separator(buffer: str) -> tuple[int, int] | None:
    matches: list[tuple[int, int]] = []
    for separator in ("\r\n\r\n", "\n\n", "\r\r"):
        index = buffer.find(separator)
        if index >= 0:
            matches.append((index, len(separator)))
    if not matches:
        return None
    return min(matches, key=lambda match: match[0])


def _parse_event(raw: str) -> tuple[str, str] | None:
    event = "message"
    data: list[str] = []
    for line in raw.splitlines():
        if not line or line.startswith(":"):
            continue
        if line.startswith("event:"):
            event = line[6:].strip()
        elif line.startswith("data:"):
            value = line[5:]
            data.append(value[1:] if value.startswith(" ") else value)
    if not data:
        return None
    return event, "\n".join(data)


@dataclass(slots=True)
class ChatStreamState:
    model: str
    include_usage: bool = True
    id: str = ""
    tool_block_indexes: dict[int, int] = field(default_factory=dict)
    tool_argument_delta_indexes: set[int] = field(default_factory=set)
    has_tool_call: bool = False

    def __post_init__(self) -> None:
        if not self.id:
            self.id = f"chatcmpl_{uuid.uuid4().hex}"


@dataclass(slots=True)
class ResponsesStreamState:
    model: str
    id: str = ""
    item_started: bool = False
    content_started: bool = False
    next_output_index: int = 0
    block_output_indexes: dict[int, int] = field(default_factory=dict)
    tool_calls: dict[int, dict[str, str]] = field(default_factory=dict)

    def __post_init__(self) -> None:
        if not self.id:
            self.id = f"resp_{uuid.uuid4().hex}"


@dataclass(slots=True)
class AnthropicStreamState:
    model: str
    message_id: str = ""
    active_block: str | None = None
    next_index: int = 0
    tool_call_blocks: dict[int, int] = field(default_factory=dict)
    tool_argument_delta_indexes: set[int] = field(default_factory=set)
    has_tool_use: bool = False

    def __post_init__(self) -> None:
        if not self.message_id:
            self.message_id = f"msg_{uuid.uuid4().hex}"


def anthropic_sse_to_chat(
    event: str,
    data: dict[str, Any],
    state: ChatStreamState,
    usage: UsageData,
) -> list[str]:
    if event == "message_start":
        return [
            sse(
                {
                    "id": state.id,
                    "object": "chat.completion.chunk",
                    "created": int(time.time()),
                    "model": state.model,
                    "choices": [
                        {
                            "index": 0,
                            "delta": {"role": "assistant"},
                            "finish_reason": None,
                        }
                    ],
                }
            )
        ]
    if event == "content_block_delta":
        delta = data.get("delta") or {}
        if delta.get("type") == "text_delta":
            return [_chat_delta(state, {"content": delta.get("text", "")})]
        if delta.get("type") == "thinking_delta":
            return [_chat_delta(state, {"reasoning_content": delta.get("thinking", "")})]
        if delta.get("type") == "input_json_delta":
            block_index = int(data.get("index") or 0)
            tool_index = state.tool_block_indexes.get(block_index, block_index)
            return [
                _chat_delta(
                    state,
                    {
                        "tool_calls": [
                            {
                                "index": tool_index,
                                "function": {"arguments": delta.get("partial_json", "")},
                            }
                        ]
                    },
                )
            ]
    if event == "content_block_start":
        block = data.get("content_block") or {}
        if block.get("type") == "tool_use":
            block_index = int(data.get("index") or 0)
            tool_index = len(state.tool_block_indexes)
            state.tool_block_indexes[block_index] = tool_index
            state.has_tool_call = True
            return [
                _chat_delta(
                    state,
                    {
                        "tool_calls": [
                            {
                                "index": tool_index,
                                "id": block.get("id"),
                                "type": "function",
                                "function": {
                                    "name": block.get("name"),
                                    "arguments": "",
                                },
                            }
                        ]
                    },
                )
            ]
    if event == "message_delta":
        stop_reason = (data.get("delta") or {}).get("stop_reason")
        if data.get("usage"):
            usage.output_tokens = int(data["usage"].get("output_tokens") or usage.output_tokens)
        if stop_reason:
            if stop_reason == "tool_use":
                reason = "tool_calls"
            elif stop_reason == "max_tokens":
                reason = "length"
            else:
                reason = "stop"
            return [_chat_done(state, reason)]
    if event == "message_stop":
        chunks = []
        if state.include_usage:
            chunks.append(
                sse(
                    {
                        "id": state.id,
                        "object": "chat.completion.chunk",
                        "created": int(time.time()),
                        "model": state.model,
                        "choices": [],
                        "usage": _usage_payload(usage),
                    }
                )
            )
        chunks.append("data: [DONE]\n\n")
        return chunks
    return []


def anthropic_sse_to_responses(
    event: str,
    data: dict[str, Any],
    state: ResponsesStreamState,
    model: str,
    usage: UsageData,
) -> list[str]:
    if event == "message_start":
        message = data.get("message") or {}
        if message.get("usage"):
            usage.input_tokens = int(message["usage"].get("input_tokens") or 0)
        return [
            sse(
                {
                    "type": "response.created",
                    "response": {
                        "id": state.id,
                        "object": "response",
                        "created_at": int(time.time()),
                        "status": "in_progress",
                        "model": model,
                    },
                },
                "response.created",
            )
        ]
    if event == "content_block_start":
        block = data.get("content_block") or {}
        block_index = int(data.get("index") or 0)
        output_index = _responses_output_index(state, block_index)
        if block.get("type") == "text":
            state.item_started = True
            state.content_started = True
            return [
                sse(
                    {
                        "type": "response.output_item.added",
                        "output_index": output_index,
                        "item": {"type": "message", "role": "assistant", "content": []},
                    },
                    "response.output_item.added",
                ),
                sse(
                    {
                        "type": "response.content_part.added",
                        "item_id": state.id,
                        "output_index": output_index,
                        "content_index": 0,
                        "part": {"type": "output_text", "text": ""},
                    },
                    "response.content_part.added",
                ),
            ]
        if block.get("type") == "thinking":
            return [
                sse(
                    {
                        "type": "response.output_item.added",
                        "output_index": output_index,
                        "item": {"type": "reasoning", "summary": []},
                    },
                    "response.output_item.added",
                )
            ]
        if block.get("type") == "tool_use":
            call_id = str(block.get("id") or f"call_{output_index}")
            arguments = json.dumps(block["input"]) if block.get("input") else ""
            state.tool_calls[block_index] = {
                "id": call_id,
                "name": str(block.get("name") or ""),
                "args": arguments,
            }
            return [
                sse(
                    {
                        "type": "response.output_item.added",
                        "output_index": output_index,
                        "item": _responses_function_item(state.tool_calls[block_index]),
                    },
                    "response.output_item.added",
                )
            ]
        if block.get("type") == "server_tool_use" and block.get("name") == "web_search":
            return [
                sse(
                    {
                        "type": "response.output_item.added",
                        "output_index": output_index,
                        "item": {
                            "type": "web_search_call",
                            "id": block.get("id"),
                            "status": "completed",
                            "action": {
                                "type": "search",
                                "query": (block.get("input") or {}).get("query", ""),
                            },
                        },
                    },
                    "response.output_item.added",
                )
            ]
    if event == "content_block_delta":
        delta = data.get("delta") or {}
        block_index = int(data.get("index") or 0)
        output_index = _responses_output_index(state, block_index)
        if delta.get("type") == "text_delta":
            return [
                sse(
                    {
                        "type": "response.output_text.delta",
                        "output_index": output_index,
                        "content_index": 0,
                        "delta": delta.get("text", ""),
                    },
                    "response.output_text.delta",
                )
            ]
        if delta.get("type") == "thinking_delta":
            return [
                sse(
                    {
                        "type": "response.reasoning_text.delta",
                        "output_index": output_index,
                        "delta": delta.get("thinking", ""),
                    },
                    "response.reasoning_text.delta",
                )
            ]
        if delta.get("type") == "input_json_delta":
            call = state.tool_calls.setdefault(
                block_index,
                {"id": str(block_index), "name": "", "args": ""},
            )
            partial_json = delta.get("partial_json") or ""
            call["args"] = f"{call.get('args', '')}{partial_json}"
            return [
                sse(
                    {
                        "type": "response.function_call_arguments.delta",
                        "item_id": call.get("id"),
                        "output_index": output_index,
                        "delta": partial_json,
                    },
                    "response.function_call_arguments.delta",
                )
            ]
    if event == "content_block_stop":
        block_index = int(data.get("index") or 0)
        call = state.tool_calls.get(block_index)
        if call:
            output_index = _responses_output_index(state, block_index)
            item = _responses_function_item(call)
            return [
                sse(
                    {
                        "type": "response.function_call_arguments.done",
                        "output_index": output_index,
                        "item": item,
                    },
                    "response.function_call_arguments.done",
                ),
                sse(
                    {
                        "type": "response.output_item.done",
                        "output_index": output_index,
                        "item": item,
                    },
                    "response.output_item.done",
                ),
            ]
    if event == "message_delta" and data.get("usage"):
        usage.output_tokens = int(data["usage"].get("output_tokens") or usage.output_tokens)
    if event == "message_stop":
        response = {
            "id": state.id,
            "object": "response",
            "created_at": int(time.time()),
            "status": "completed",
            "model": model,
            "usage": {
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "total_tokens": usage.input_tokens + usage.output_tokens,
                "input_tokens_details": {"cached_tokens": usage.cache_read_input_tokens},
                "output_tokens_details": {"reasoning_tokens": usage.reasoning_output_tokens},
            },
        }
        return [sse({"type": "response.completed", "response": response}, "response.completed")]
    return []


def responses_sse_to_chat(
    event: str,
    data: dict[str, Any],
    state: ChatStreamState,
) -> list[str]:
    if event == "response.created":
        return [
            sse(
                {
                    "id": state.id,
                    "object": "chat.completion.chunk",
                    "created": int(time.time()),
                    "model": state.model,
                    "choices": [
                        {
                            "index": 0,
                            "delta": {"role": "assistant"},
                            "finish_reason": None,
                        }
                    ],
                }
            )
        ]
    if event in ("response.output_text.delta", "response.refusal.delta"):
        return [_chat_delta(state, {"content": data.get("delta", "")})]
    if event in ("response.reasoning_summary_text.delta", "response.reasoning_text.delta"):
        return [_chat_delta(state, {"reasoning_content": data.get("delta", "")})]
    if event == "response.output_item.added":
        item = data.get("item") or {}
        if item.get("type") == "function_call":
            state.has_tool_call = True
            return [
                _chat_delta(
                    state,
                    {
                        "tool_calls": [
                            {
                                "index": int(data.get("output_index") or 0),
                                "id": item.get("call_id") or item.get("id"),
                                "type": "function",
                                "function": {
                                    "name": item.get("name"),
                                    "arguments": item.get("arguments") or "",
                                },
                            }
                        ]
                    },
                )
            ]
    if event == "response.function_call_arguments.delta":
        state.has_tool_call = True
        output_index = int(data.get("output_index") or 0)
        state.tool_argument_delta_indexes.add(output_index)
        return [
            _chat_delta(
                state,
                {
                    "tool_calls": [
                        {
                            "index": output_index,
                            "function": {"arguments": data.get("delta") or ""},
                        }
                    ]
                },
            )
        ]
    if event == "response.function_call_arguments.done":
        item = data.get("item") or data
        output_index = int(data.get("output_index") or item.get("output_index") or 0)
        if output_index in state.tool_argument_delta_indexes:
            return []
        state.has_tool_call = True
        return [
            _chat_delta(
                state,
                {
                    "tool_calls": [
                        {
                            "index": output_index,
                            "function": {"arguments": item.get("arguments") or ""},
                        }
                    ]
                },
            )
        ]
    if event in ("response.completed", "response.incomplete"):
        reason = (
            "tool_calls"
            if state.has_tool_call
            else "length"
            if event == "response.incomplete"
            else "stop"
        )
        chunks = [_chat_done(state, reason)]
        response = data.get("response") or data
        usage = response.get("usage")
        if state.include_usage and usage:
            chunks.append(
                sse(
                    {
                        "id": state.id,
                        "object": "chat.completion.chunk",
                        "created": int(time.time()),
                        "model": state.model,
                        "choices": [],
                        "usage": _responses_usage_to_chat(usage),
                    }
                )
            )
        chunks.append("data: [DONE]\n\n")
        return chunks
    return []


def responses_sse_to_anthropic(
    event: str,
    data: dict[str, Any],
    state: AnthropicStreamState,
) -> list[str]:
    if event == "response.created":
        return [
            sse(
                {
                    "type": "message_start",
                    "message": {
                        "id": state.message_id,
                        "type": "message",
                        "role": "assistant",
                        "model": state.model,
                        "content": [],
                        "stop_reason": None,
                        "stop_sequence": None,
                        "usage": {"input_tokens": 0, "output_tokens": 0},
                    },
                },
                "message_start",
            ),
        ]
    if event in ("response.output_text.delta", "response.refusal.delta"):
        chunks = _ensure_anthropic_block(state, "text")
        chunks.append(
            sse(
                {
                    "type": "content_block_delta",
                    "index": state.next_index - 1,
                    "delta": {"type": "text_delta", "text": data.get("delta", "")},
                },
                "content_block_delta",
            )
        )
        return chunks
    if event in ("response.reasoning_summary_text.delta", "response.reasoning_text.delta"):
        chunks = _ensure_anthropic_block(state, "thinking")
        chunks.append(
            sse(
                {
                    "type": "content_block_delta",
                    "index": state.next_index - 1,
                    "delta": {"type": "thinking_delta", "thinking": data.get("delta", "")},
                },
                "content_block_delta",
            )
        )
        return chunks
    if event == "response.output_item.added":
        item = data.get("item") or {}
        if item.get("type") == "function_call":
            chunks: list[str] = []
            if state.active_block is not None:
                chunks.append(
                    sse(
                        {"type": "content_block_stop", "index": state.next_index - 1},
                        "content_block_stop",
                    )
                )
            index = state.next_index
            state.next_index += 1
            state.active_block = "tool_use"
            state.has_tool_use = True
            state.tool_call_blocks[int(data.get("output_index") or 0)] = index
            chunks.append(
                sse(
                    {
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "tool_use",
                            "id": item.get("call_id") or item.get("id"),
                            "name": item.get("name"),
                            "input": {},
                        },
                    },
                    "content_block_start",
                )
            )
            return chunks
        if item.get("type") == "web_search_call":
            chunks = _start_anthropic_server_tool_use(state, data)
            return chunks
    if event == "response.output_item.done":
        item = data.get("item") or {}
        if item.get("type") == "web_search_call":
            return _start_anthropic_server_tool_use(state, data)
    if event == "response.function_call_arguments.delta":
        output_index = int(data.get("output_index") or 0)
        state.tool_argument_delta_indexes.add(output_index)
        block_index = state.tool_call_blocks.get(output_index, state.next_index - 1)
        return [
            sse(
                {
                    "type": "content_block_delta",
                    "index": block_index,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": data.get("delta") or "",
                    },
                },
                "content_block_delta",
            )
        ]
    if event == "response.function_call_arguments.done":
        item = data.get("item") or data
        output_index = int(data.get("output_index") or item.get("output_index") or 0)
        if output_index in state.tool_argument_delta_indexes:
            return []
        block_index = state.tool_call_blocks.get(output_index, state.next_index - 1)
        return [
            sse(
                {
                    "type": "content_block_delta",
                    "index": block_index,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": item.get("arguments") or "",
                    },
                },
                "content_block_delta",
            )
        ]
    if event in ("response.completed", "response.incomplete"):
        response = data.get("response") or data
        usage = response.get("usage") or {}
        chunks: list[str] = []
        if state.active_block is not None:
            chunks.append(
                sse(
                    {"type": "content_block_stop", "index": state.next_index - 1},
                    "content_block_stop",
                )
            )
            state.active_block = None
        chunks.extend(
            [
                sse(
                    {
                        "type": "message_delta",
                        "delta": {
                            "stop_reason": (
                                "tool_use"
                                if state.has_tool_use
                                else "max_tokens"
                                if event == "response.incomplete"
                                else "end_turn"
                            ),
                            "stop_sequence": None,
                        },
                        "usage": {"output_tokens": usage.get("output_tokens", 0)},
                    },
                    "message_delta",
                ),
                sse({"type": "message_stop"}, "message_stop"),
            ]
        )
        return chunks
    return []


def _start_anthropic_server_tool_use(
    state: AnthropicStreamState,
    data: dict[str, Any],
) -> list[str]:
    item = data.get("item") or {}
    output_index = int(data.get("output_index") or 0)
    if output_index in state.tool_call_blocks:
        return []

    chunks: list[str] = []
    if state.active_block is not None:
        chunks.append(
            sse({"type": "content_block_stop", "index": state.next_index - 1}, "content_block_stop")
        )
    index = state.next_index
    state.next_index += 1
    state.active_block = "server_tool_use"
    state.tool_call_blocks[output_index] = index
    action = item.get("action") or {}
    chunks.append(
        sse(
            {
                "type": "content_block_start",
                "index": index,
                "content_block": {
                    "type": "server_tool_use",
                    "id": item.get("id"),
                    "name": "web_search",
                    "input": {"query": web_search_query_from_action(action)},
                },
            },
            "content_block_start",
        )
    )
    return chunks


def _responses_output_index(state: ResponsesStreamState, block_index: int) -> int:
    if block_index not in state.block_output_indexes:
        state.block_output_indexes[block_index] = state.next_output_index
        state.next_output_index += 1
    return state.block_output_indexes[block_index]


def _responses_function_item(call: dict[str, str]) -> dict[str, str]:
    return {
        "type": "function_call",
        "id": call.get("id", ""),
        "call_id": call.get("id", ""),
        "name": call.get("name", ""),
        "arguments": call.get("args", ""),
    }


def _ensure_anthropic_block(state: AnthropicStreamState, block_type: str) -> list[str]:
    if state.active_block == block_type:
        return []

    chunks: list[str] = []
    if state.active_block is not None:
        chunks.append(
            sse({"type": "content_block_stop", "index": state.next_index - 1}, "content_block_stop")
        )

    index = state.next_index
    state.next_index += 1
    state.active_block = block_type
    if block_type == "thinking":
        content_block = {"type": "thinking", "thinking": ""}
    else:
        content_block = {"type": "text", "text": ""}
    chunks.append(
        sse(
            {
                "type": "content_block_start",
                "index": index,
                "content_block": content_block,
            },
            "content_block_start",
        )
    )
    return chunks


def _chat_delta(state: ChatStreamState, delta: dict[str, Any]) -> str:
    return sse(
        {
            "id": state.id,
            "object": "chat.completion.chunk",
            "created": int(time.time()),
            "model": state.model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": None}],
        }
    )


def _chat_done(state: ChatStreamState, reason: str) -> str:
    return sse(
        {
            "id": state.id,
            "object": "chat.completion.chunk",
            "created": int(time.time()),
            "model": state.model,
            "choices": [{"index": 0, "delta": {}, "finish_reason": reason}],
        }
    )


def _usage_payload(usage: UsageData) -> dict[str, Any]:
    return {
        "prompt_tokens": usage.input_tokens,
        "completion_tokens": usage.output_tokens,
        "total_tokens": usage.input_tokens + usage.output_tokens,
        "prompt_tokens_details": {"cached_tokens": usage.cache_read_input_tokens},
        "completion_tokens_details": {"reasoning_tokens": usage.reasoning_output_tokens},
    }


def _responses_usage_to_chat(usage: dict[str, Any]) -> dict[str, Any]:
    prompt = int(usage.get("input_tokens") or 0)
    completion = int(usage.get("output_tokens") or 0)
    return {
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
        "prompt_tokens_details": usage.get("input_tokens_details") or {"cached_tokens": 0},
        "completion_tokens_details": usage.get("output_tokens_details") or {"reasoning_tokens": 0},
    }


async def drain_responses_sse(upstream: UpstreamResponse) -> ResponsesDrain:
    drain = ResponsesDrain()
    try:
        async for event, raw in iter_sse_events(upstream):
            if raw == "[DONE]":
                break
            try:
                data = json.loads(raw)
            except json.JSONDecodeError:
                continue
            update_drain_from_response_event(drain, event, data)
    finally:
        await upstream.aclose()
    return drain


async def transformed_sse(
    upstream: UpstreamResponse,
    transform: Callable[[str, dict[str, Any], UsageData], list[str]],
    on_success: Callable[[UsageData], None],
    on_failure: Callable[[str], None],
) -> AsyncIterator[bytes]:
    usage = UsageData()
    completed = False
    try:
        async for event, raw in iter_sse_events(upstream):
            if raw == "[DONE]":
                completed = True
                yield b"data: [DONE]\n\n"
                break
            try:
                data = json.loads(raw)
            except json.JSONDecodeError:
                continue
            _update_anthropic_usage(event, data, usage)
            for chunk in transform(event, data, usage):
                yield chunk.encode("utf-8")
            if event in ("message_stop", "response.completed"):
                completed = True
        if completed:
            on_success(usage)
        else:
            on_failure("stream terminated before completion")
    finally:
        await upstream.aclose()


async def passthrough_sse(
    upstream: UpstreamResponse,
    on_success: Callable[[UsageData], None],
    on_failure: Callable[[str], None],
) -> AsyncIterator[bytes]:
    usage = UsageData()
    completed = False
    try:
        async for event, raw in iter_sse_events(upstream):
            if raw == "[DONE]":
                completed = True
                yield b"data: [DONE]\n\n"
                break
            try:
                data = json.loads(raw)
            except json.JSONDecodeError:
                yield sse(raw, event if event != "message" else None).encode("utf-8")
                continue
            if event == "response.completed":
                response = data.get("response") or data
                usage = usage_from_responses(response.get("usage"))
                completed = True
            elif event == "message_stop":
                completed = True
            _update_anthropic_usage(event, data, usage)
            yield sse(data, event if event != "message" else None).encode("utf-8")
        if completed:
            on_success(usage)
        else:
            on_failure("stream terminated before completion")
    finally:
        await upstream.aclose()


def _update_anthropic_usage(event: str, data: dict[str, Any], usage: UsageData) -> None:
    if event == "message_start":
        message = data.get("message") or {}
        msg_usage = message.get("usage") or {}
        usage.input_tokens = int(msg_usage.get("input_tokens") or usage.input_tokens)
        usage.cache_creation_input_tokens = int(
            msg_usage.get("cache_creation_input_tokens") or usage.cache_creation_input_tokens
        )
        usage.cache_read_input_tokens = int(
            msg_usage.get("cache_read_input_tokens") or usage.cache_read_input_tokens
        )
    elif event == "message_delta":
        msg_usage = data.get("usage") or {}
        usage.output_tokens = int(msg_usage.get("output_tokens") or usage.output_tokens)
    elif event == "response.completed":
        response = data.get("response") or data
        response_usage = response.get("usage")
        if response_usage:
            next_usage = usage_from_responses(response_usage)
            usage.input_tokens = next_usage.input_tokens
            usage.output_tokens = next_usage.output_tokens
            usage.cache_read_input_tokens = next_usage.cache_read_input_tokens
            usage.reasoning_output_tokens = next_usage.reasoning_output_tokens
