from __future__ import annotations

import json
import time
import uuid
from dataclasses import dataclass, field
from typing import Any

from .types import UsageData

MODEL_ALIASES = {
    "opus": "claude-opus-4-7",
    "sonnet": "claude-sonnet-4-6",
    "haiku": "claude-haiku-4-5-20251001",
}
ANTHROPIC_WEB_SEARCH_TOOL_TYPE = "web_search_20250305"
ANTHROPIC_WEB_SEARCH_TOOL_TYPES = {
    "web_search_20250305",
    "web_search_20260209",
}


def resolve_model(model: str | None) -> str:
    if not model:
        return "claude-sonnet-4-6"
    return MODEL_ALIASES.get(model, model)


def _thinking_from_effort(effort: str | None, summary: str | None = None) -> dict[str, Any] | None:
    if not effort:
        return None
    budgets = {"low": 4_096, "medium": 8_192, "high": 24_576}
    thinking: dict[str, Any] = {
        "type": "enabled",
        "budget_tokens": budgets.get(effort, budgets["medium"]),
    }
    if summary:
        thinking["display"] = "summarized"
    return thinking


def _effort_from_budget(budget: int | None) -> str:
    if not budget:
        return "medium"
    if budget <= 4_096:
        return "low"
    if budget <= 8_192:
        return "medium"
    return "high"


def _text_from_content(content: Any) -> str:
    if content is None:
        return ""
    if isinstance(content, str):
        return content
    if isinstance(content, dict):
        if "text" in content:
            return str(content.get("text") or "")
        if content.get("type") in ("input_text", "output_text"):
            return str(content.get("text") or "")
        if content.get("type") == "tool_result":
            return str(content.get("content") or "")
        return ""
    if isinstance(content, list):
        pieces: list[str] = []
        for block in content:
            if isinstance(block, str):
                pieces.append(block)
            elif isinstance(block, dict):
                if "text" in block:
                    pieces.append(str(block.get("text") or ""))
                elif block.get("type") in ("input_text", "output_text"):
                    pieces.append(str(block.get("text") or ""))
                elif block.get("type") == "tool_result":
                    pieces.append(str(block.get("content") or ""))
        return "".join(pieces)
    return str(content)


def _openai_content_to_anthropic(content: Any) -> Any:
    if isinstance(content, str) or content is None:
        return content or ""
    if not isinstance(content, list):
        return str(content)
    blocks: list[dict[str, Any]] = []
    for item in content:
        if isinstance(item, str):
            blocks.append({"type": "text", "text": item})
            continue
        if not isinstance(item, dict):
            blocks.append({"type": "text", "text": str(item)})
            continue
        typ = item.get("type")
        if typ in ("text", "input_text"):
            blocks.append({"type": "text", "text": item.get("text", "")})
        elif typ == "image_url":
            image = _image_url_to_anthropic(_image_url_value(item.get("image_url")))
            if image:
                blocks.append(image)
        elif typ == "input_image":
            image = _image_url_to_anthropic(_image_url_value(item.get("image_url")))
            if image:
                blocks.append(image)
    return blocks if blocks else ""


def _image_url_value(value: Any) -> str:
    if isinstance(value, dict):
        return str(value.get("url") or "")
    return str(value or "")


def _image_url_to_anthropic(url: str) -> dict[str, Any] | None:
    if not url:
        return None
    if url.startswith("data:") and ";base64," in url:
        media_type = url.split(":", 1)[1].split(";", 1)[0]
        data = url.split(";base64,", 1)[1]
        return {
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data},
        }
    return {"type": "image", "source": {"type": "url", "url": url}}


def _anthropic_image_to_responses(block: dict[str, Any]) -> dict[str, Any] | None:
    source = block.get("source") or {}
    if not isinstance(source, dict):
        return None
    source_type = source.get("type")
    if source_type == "url":
        url = str(source.get("url") or "")
        if not url:
            return None
        return {"type": "input_image", "image_url": url}
    if source_type == "base64":
        media_type = str(source.get("media_type") or "image/png")
        data = str(source.get("data") or "")
        if not data:
            return None
        return {"type": "input_image", "image_url": f"data:{media_type};base64,{data}"}
    return None


def _anthropic_system_from_messages(messages: list[dict[str, Any]]) -> list[dict[str, str]]:
    system: list[dict[str, str]] = []
    for message in messages:
        if message.get("role") in ("system", "developer"):
            text = _text_from_content(message.get("content"))
            if text:
                system.append({"type": "text", "text": text})
    return system


def _anthropic_web_search_tool(tool: dict[str, Any]) -> dict[str, Any]:
    out: dict[str, Any] = {
        "type": tool.get("type")
        if tool.get("type") in ANTHROPIC_WEB_SEARCH_TOOL_TYPES
        else ANTHROPIC_WEB_SEARCH_TOOL_TYPE,
        "name": tool.get("name") or "web_search",
    }
    if "max_uses" in tool:
        out["max_uses"] = tool["max_uses"]
    filters = tool.get("filters") or {}
    allowed_domains = tool.get("allowed_domains") or filters.get("allowed_domains")
    blocked_domains = tool.get("blocked_domains") or filters.get("blocked_domains")
    if allowed_domains:
        out["allowed_domains"] = allowed_domains
    if blocked_domains:
        out["blocked_domains"] = blocked_domains
    if tool.get("user_location"):
        out["user_location"] = tool["user_location"]
    return out


def _openai_tool_to_anthropic(tool: Any) -> dict[str, Any] | None:
    if not isinstance(tool, dict):
        return None
    tool_type = tool.get("type")
    if tool_type in ("web_search", "web_search_preview", *ANTHROPIC_WEB_SEARCH_TOOL_TYPES):
        return _anthropic_web_search_tool(tool)
    if tool_type == "function" and tool.get("name"):
        return {
            "name": tool.get("name"),
            "description": tool.get("description", ""),
            "input_schema": tool.get("parameters") or {"type": "object"},
        }
    function = tool.get("function")
    if not isinstance(function, dict):
        return None
    return {
        "name": function.get("name"),
        "description": function.get("description", ""),
        "input_schema": function.get("parameters") or {"type": "object"},
    }


def _responses_tool_from_chat_tool(tool: Any) -> dict[str, Any] | None:
    if not isinstance(tool, dict):
        return None
    tool_type = tool.get("type")
    if tool_type and tool_type != "function":
        return dict(tool)
    fn = tool.get("function") or {}
    if not isinstance(fn, dict) or not fn.get("name"):
        return None
    out = {
        "type": "function",
        "name": fn.get("name"),
        "description": fn.get("description", ""),
        "parameters": fn.get("parameters") or {"type": "object"},
    }
    if "strict" in fn:
        out["strict"] = fn["strict"]
    return out


def _anthropic_tool_to_responses(tool: dict[str, Any]) -> dict[str, Any]:
    if tool.get("type") in ANTHROPIC_WEB_SEARCH_TOOL_TYPES:
        out: dict[str, Any] = {"type": "web_search"}
        filters: dict[str, Any] = {}
        if tool.get("allowed_domains"):
            filters["allowed_domains"] = tool["allowed_domains"]
        if tool.get("blocked_domains"):
            filters["blocked_domains"] = tool["blocked_domains"]
        if filters:
            out["filters"] = filters
        if tool.get("user_location"):
            out["user_location"] = tool["user_location"]
        return out
    return {
        "type": "function",
        "name": tool.get("name"),
        "description": tool.get("description", ""),
        "parameters": tool.get("input_schema") or {"type": "object"},
    }


def _anthropic_tool_choice(body: dict[str, Any], has_tools: bool) -> dict[str, Any] | None:
    if not has_tools:
        return None
    if "tool_choice" not in body and body.get("parallel_tool_calls") is not False:
        return None
    choice = body.get("tool_choice", "auto")
    out: dict[str, Any] | None = None
    if choice == "auto":
        out = {"type": "auto"}
    elif choice == "required":
        out = {"type": "any"}
    elif choice == "none":
        out = {"type": "none"}
    elif isinstance(choice, dict):
        choice_type = choice.get("type")
        if choice_type in ("web_search", "web_search_preview"):
            out = {"type": "tool", "name": "web_search"}
        elif choice_type == "function":
            function = choice.get("function") or {}
            out = {"type": "tool", "name": choice.get("name") or function.get("name")}
        elif choice_type in ("auto", "any", "none", "tool"):
            out = dict(choice)
    if out is None:
        out = {"type": "auto"}
    if body.get("parallel_tool_calls") is False:
        out["disable_parallel_tool_use"] = True
    return out


def _responses_tool_choice_from_chat(choice: Any) -> Any:
    if not isinstance(choice, dict):
        return choice
    if choice.get("type") == "function":
        function = choice.get("function") or {}
        return {"type": "function", "name": choice.get("name") or function.get("name")}
    return choice


def _responses_tool_choice_from_anthropic(choice: Any) -> Any:
    if not isinstance(choice, dict):
        return choice
    choice_type = choice.get("type")
    if choice_type == "auto":
        return "auto"
    if choice_type == "any":
        return "required"
    if choice_type == "none":
        return "none"
    if choice_type == "tool":
        name = choice.get("name")
        if name == "web_search":
            return {"type": "web_search"}
        return {"type": "function", "name": name}
    return choice


def _anthropic_citations_to_openai(citations: list[dict[str, Any]]) -> list[dict[str, Any]]:
    annotations: list[dict[str, Any]] = []
    for citation in citations:
        if citation.get("type") != "web_search_result_location":
            continue
        annotation = {
            "type": "url_citation",
            "url": citation.get("url"),
            "title": citation.get("title"),
        }
        if "start_index" in citation:
            annotation["start_index"] = citation["start_index"]
        if "end_index" in citation:
            annotation["end_index"] = citation["end_index"]
        annotations.append(annotation)
    return annotations


def _openai_annotations_to_anthropic(annotations: list[dict[str, Any]]) -> list[dict[str, Any]]:
    citations: list[dict[str, Any]] = []
    for annotation in annotations:
        if annotation.get("type") != "url_citation":
            continue
        citation = {
            "type": "web_search_result_location",
            "url": annotation.get("url"),
            "title": annotation.get("title"),
        }
        if "start_index" in annotation:
            citation["start_index"] = annotation["start_index"]
        if "end_index" in annotation:
            citation["end_index"] = annotation["end_index"]
        citations.append(citation)
    return citations


def web_search_query_from_action(action: dict[str, Any]) -> str:
    query = action.get("query")
    if isinstance(query, str):
        return query
    queries = action.get("queries")
    if isinstance(queries, list):
        return "\n".join(str(item) for item in queries if item)
    return ""


def _responses_input_to_openai_messages(input_items: Any) -> list[dict[str, Any]]:
    if isinstance(input_items, str):
        return [{"role": "user", "content": input_items}]
    messages: list[dict[str, Any]] = []
    for item in input_items or []:
        if not isinstance(item, dict):
            messages.append({"role": "user", "content": str(item)})
            continue
        item_type = item.get("type")
        if item.get("role"):
            messages.append(item)
        elif item_type == "function_call":
            messages.append(
                {
                    "role": "assistant",
                    "tool_calls": [
                        {
                            "id": item.get("call_id"),
                            "type": "function",
                            "function": {
                                "name": item.get("name"),
                                "arguments": item.get("arguments") or "{}",
                            },
                        }
                    ],
                }
            )
        elif item_type == "function_call_output":
            messages.append(
                {
                    "role": "tool",
                    "tool_call_id": item.get("call_id"),
                    "content": item.get("output") or "",
                }
            )
    return messages


def openai_to_anthropic(body: dict[str, Any]) -> dict[str, Any]:
    messages = body.get("messages") or []
    result: dict[str, Any] = {
        "model": resolve_model(body.get("model")),
        "messages": [],
        "max_tokens": body.get("max_completion_tokens") or body.get("max_tokens") or 8192,
    }
    if "stream" in body:
        result["stream"] = bool(body["stream"])
    for key in ("temperature", "top_p"):
        if key in body:
            result[key] = body[key]
    if "stop" in body:
        result["stop_sequences"] = (
            body["stop"] if isinstance(body["stop"], list) else [body["stop"]]
        )

    system = _anthropic_system_from_messages(messages)
    if system:
        result["system"] = system

    for message in messages:
        role = message.get("role")
        if role in ("system", "developer"):
            continue
        if role == "tool":
            result["messages"].append(
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": message.get("tool_call_id"),
                            "content": _text_from_content(message.get("content")),
                        }
                    ],
                }
            )
            continue
        if role == "assistant" and message.get("tool_calls"):
            content: list[dict[str, Any]] = []
            text = _text_from_content(message.get("content"))
            if text:
                content.append({"type": "text", "text": text})
            for call in message.get("tool_calls") or []:
                function = call.get("function") or {}
                args = function.get("arguments") or "{}"
                try:
                    parsed_args = json.loads(args)
                except Exception:
                    parsed_args = args
                content.append(
                    {
                        "type": "tool_use",
                        "id": call.get("id"),
                        "name": function.get("name"),
                        "input": parsed_args,
                    }
                )
            result["messages"].append({"role": "assistant", "content": content})
            continue
        if role in ("user", "assistant"):
            result["messages"].append(
                {"role": role, "content": _openai_content_to_anthropic(message.get("content"))}
            )

    thinking = _thinking_from_effort(body.get("reasoning_effort"))
    if thinking:
        result["thinking"] = thinking

    available_tools = list(body.get("tools") or []) + list(body.get("responses_tools") or [])
    if available_tools:
        tools = []
        for tool in available_tools:
            translated_tool = _openai_tool_to_anthropic(tool)
            if translated_tool:
                tools.append(translated_tool)
        if tools:
            result["tools"] = tools

    tool_choice = _anthropic_tool_choice(body, bool(result.get("tools")))
    if tool_choice:
        result["tool_choice"] = tool_choice

    response_format = body.get("response_format") or {}
    if response_format.get("type") == "json_schema":
        schema = response_format.get("json_schema") or {}
        result["output_config"] = {
            "format": {
                "type": "json_schema",
                "name": schema.get("name", "response"),
                "schema": schema.get("schema") or {},
                **({"strict": schema.get("strict")} if "strict" in schema else {}),
            }
        }
    elif response_format.get("type") == "json_object":
        result["output_config"] = {"format": {"type": "json_object"}}

    return result


def _finish_reason(stop_reason: str | None, has_tool_calls: bool = False) -> str:
    if has_tool_calls or stop_reason == "tool_use":
        return "tool_calls"
    if stop_reason == "max_tokens":
        return "length"
    if stop_reason in ("stop_sequence", "end_turn", None):
        return "stop"
    return stop_reason


def _usage_to_openai(usage: dict[str, Any] | None) -> dict[str, Any]:
    usage = usage or {}
    prompt = int(usage.get("input_tokens") or 0)
    completion = int(usage.get("output_tokens") or 0)
    cached = int(usage.get("cache_read_input_tokens") or 0)
    reasoning = int((usage.get("output_tokens_details") or {}).get("reasoning_tokens") or 0)
    return {
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
        "prompt_tokens_details": {"cached_tokens": cached},
        "completion_tokens_details": {"reasoning_tokens": reasoning},
    }


def anthropic_to_openai(payload: dict[str, Any], model: str) -> dict[str, Any]:
    text_parts: list[str] = []
    tool_calls: list[dict[str, Any]] = []
    for block in payload.get("content") or []:
        if block.get("type") == "text":
            text_parts.append(block.get("text", ""))
        elif block.get("type") == "tool_use":
            tool_calls.append(
                {
                    "id": block.get("id"),
                    "type": "function",
                    "function": {
                        "name": block.get("name"),
                        "arguments": json.dumps(block.get("input") or {}),
                    },
                }
            )
    message: dict[str, Any] = {"role": "assistant", "content": "".join(text_parts)}
    if tool_calls:
        message["tool_calls"] = tool_calls
    return {
        "id": payload.get("id") or f"chatcmpl_{uuid.uuid4().hex}",
        "object": "chat.completion",
        "created": int(time.time()),
        "model": model,
        "choices": [
            {
                "index": 0,
                "message": message,
                "finish_reason": _finish_reason(payload.get("stop_reason"), bool(tool_calls)),
            }
        ],
        "usage": _usage_to_openai(payload.get("usage")),
    }


def responses_to_anthropic(body: dict[str, Any]) -> dict[str, Any]:
    input_items = body.get("input", body.get("messages") or [])
    messages = _responses_input_to_openai_messages(input_items)
    pseudo_chat = {
        "model": body.get("model"),
        "messages": messages,
        "stream": body.get("stream", False),
        "temperature": body.get("temperature"),
        "top_p": body.get("top_p"),
        "max_completion_tokens": body.get("max_output_tokens"),
        "tools": body.get("tools"),
        "tool_choice": body.get("tool_choice"),
        "parallel_tool_calls": body.get("parallel_tool_calls"),
    }
    result = openai_to_anthropic({k: v for k, v in pseudo_chat.items() if v is not None})
    if body.get("instructions"):
        result["system"] = [{"type": "text", "text": body["instructions"]}]
    reasoning = body.get("reasoning") or {}
    thinking = _thinking_from_effort(reasoning.get("effort"), reasoning.get("summary"))
    if thinking:
        result["thinking"] = thinking
    text_format = (body.get("text") or {}).get("format") or {}
    if text_format.get("type") in ("json_schema", "json_object"):
        result["output_config"] = {"format": text_format}
    return result


def anthropic_to_responses(payload: dict[str, Any], model: str) -> dict[str, Any]:
    output: list[dict[str, Any]] = []
    text = ""
    output_text_parts: list[str] = []
    annotations: list[dict[str, Any]] = []

    def flush_text() -> None:
        nonlocal text, annotations
        if not text:
            return
        output_text_parts.append(text)
        content: dict[str, Any] = {"type": "output_text", "text": text}
        if annotations:
            content["annotations"] = annotations
        output.append(
            {
                "type": "message",
                "role": "assistant",
                "content": [content],
            }
        )
        text = ""
        annotations = []

    for block in payload.get("content") or []:
        if block.get("type") == "thinking":
            flush_text()
            output.append(
                {
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": block.get("thinking", "")}],
                }
            )
        elif block.get("type") == "text":
            text += block.get("text", "")
            annotations.extend(_anthropic_citations_to_openai(block.get("citations") or []))
        elif block.get("type") == "tool_use":
            flush_text()
            output.append(
                {
                    "type": "function_call",
                    "call_id": block.get("id"),
                    "name": block.get("name"),
                    "arguments": json.dumps(block.get("input") or {}),
                }
            )
        elif block.get("type") == "server_tool_use" and block.get("name") == "web_search":
            flush_text()
            output.append(
                {
                    "type": "web_search_call",
                    "id": block.get("id"),
                    "status": "completed",
                    "action": {
                        "type": "search",
                        "query": (block.get("input") or {}).get("query", ""),
                    },
                }
            )
    flush_text()
    usage = payload.get("usage") or {}
    input_tokens = int(usage.get("input_tokens") or 0)
    output_tokens = int(usage.get("output_tokens") or 0)
    return {
        "id": payload.get("id") or f"resp_{uuid.uuid4().hex}",
        "object": "response",
        "created_at": int(time.time()),
        "status": "incomplete" if payload.get("stop_reason") == "max_tokens" else "completed",
        "model": model,
        "output": output,
        "output_text": "".join(output_text_parts),
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
            "input_tokens_details": {
                "cached_tokens": int(usage.get("cache_read_input_tokens") or 0)
            },
            "output_tokens_details": {"reasoning_tokens": 0},
        },
    }


def chat_to_responses_request(body: dict[str, Any]) -> dict[str, Any]:
    instructions: list[str] = []
    input_items: list[dict[str, Any]] = []
    for message in body.get("messages") or []:
        role = message.get("role")
        if role in ("system", "developer"):
            text = _text_from_content(message.get("content"))
            if text:
                instructions.append(text)
            continue
        if role == "tool":
            input_items.append(
                {
                    "type": "function_call_output",
                    "call_id": message.get("tool_call_id"),
                    "output": _text_from_content(message.get("content")),
                }
            )
            continue
        if role == "assistant" and message.get("tool_calls"):
            text = _text_from_content(message.get("content"))
            if text:
                input_items.append({"role": "assistant", "content": text})
            for call in message.get("tool_calls") or []:
                fn = call.get("function") or {}
                input_items.append(
                    {
                        "type": "function_call",
                        "call_id": call.get("id"),
                        "name": fn.get("name"),
                        "arguments": fn.get("arguments") or "{}",
                    }
                )
            continue
        if role in ("user", "assistant"):
            input_items.append({"role": role, "content": message.get("content") or ""})

    out: dict[str, Any] = {
        "model": body.get("model"),
        "input": input_items,
    }
    if instructions:
        out["instructions"] = "\n\n".join(instructions)
    if "stream" in body:
        out["stream"] = body["stream"]
    for src, dst in (("temperature", "temperature"), ("top_p", "top_p")):
        if src in body:
            out[dst] = body[src]
    if body.get("max_completion_tokens") or body.get("max_tokens"):
        out["max_output_tokens"] = body.get("max_completion_tokens") or body.get("max_tokens")
    if body.get("reasoning_effort"):
        out["reasoning"] = {"effort": body["reasoning_effort"]}
    response_tools = []
    for tool in body.get("tools") or []:
        translated_tool = _responses_tool_from_chat_tool(tool)
        if translated_tool:
            response_tools.append(translated_tool)
    for tool in body.get("responses_tools") or []:
        if isinstance(tool, dict):
            response_tools.append(dict(tool))
    if response_tools:
        out["tools"] = response_tools
    if "responses_tool_choice" in body:
        out["tool_choice"] = body["responses_tool_choice"]
    elif "tool_choice" in body:
        out["tool_choice"] = _responses_tool_choice_from_chat(body["tool_choice"])
    if "parallel_tool_calls" in body:
        out["parallel_tool_calls"] = body["parallel_tool_calls"]
    response_format = body.get("response_format") or {}
    if response_format.get("type") == "json_schema":
        schema = response_format.get("json_schema") or {}
        out["text"] = {
            "format": {
                "type": "json_schema",
                "name": schema.get("name", "response"),
                "schema": schema.get("schema") or {},
                **({"strict": schema.get("strict")} if "strict" in schema else {}),
            }
        }
    elif response_format.get("type") == "json_object":
        out["text"] = {"format": {"type": "json_object"}}
    return out


def anthropic_to_responses_request(body: dict[str, Any]) -> dict[str, Any]:
    out: dict[str, Any] = {
        "model": body.get("model"),
        "input": [],
    }
    if "stream" in body:
        out["stream"] = body["stream"]
    if "max_tokens" in body:
        out["max_output_tokens"] = body["max_tokens"]
    for key in ("temperature", "top_p"):
        if key in body:
            out[key] = body[key]
    system = body.get("system")
    if isinstance(system, str):
        out["instructions"] = system
    elif isinstance(system, list):
        out["instructions"] = "\n\n".join(_text_from_content(block) for block in system if block)
    thinking = body.get("thinking") or {}
    if thinking.get("type") == "enabled":
        out["reasoning"] = {"effort": _effort_from_budget(thinking.get("budget_tokens"))}

    for message in body.get("messages") or []:
        role = message.get("role")
        content = message.get("content")
        if isinstance(content, list):
            content_parts: list[dict[str, Any]] = []

            def flush_message(message_role: str | None) -> None:
                nonlocal content_parts
                if not content_parts:
                    return
                if all(part.get("type") == "input_text" for part in content_parts):
                    text = "".join(str(part.get("text") or "") for part in content_parts)
                    out["input"].append({"role": message_role, "content": text})
                else:
                    out["input"].append({"role": message_role, "content": content_parts})
                content_parts = []

            for block in content:
                typ = block.get("type") if isinstance(block, dict) else None
                if typ == "text":
                    content_parts.append({"type": "input_text", "text": block.get("text", "")})
                elif typ == "image":
                    image = _anthropic_image_to_responses(block)
                    if image:
                        content_parts.append(image)
                elif typ == "tool_use":
                    flush_message(role)
                    out["input"].append(
                        {
                            "type": "function_call",
                            "call_id": block.get("id"),
                            "name": block.get("name"),
                            "arguments": json.dumps(block.get("input") or {}),
                        }
                    )
                elif typ == "tool_result":
                    flush_message(role)
                    out["input"].append(
                        {
                            "type": "function_call_output",
                            "call_id": block.get("tool_use_id"),
                            "output": _text_from_content(block.get("content")),
                        }
                    )
            flush_message(role)
        else:
            out["input"].append({"role": role, "content": content or ""})

    if body.get("tools"):
        out["tools"] = [_anthropic_tool_to_responses(tool) for tool in body["tools"]]
    if "tool_choice" in body:
        out["tool_choice"] = _responses_tool_choice_from_anthropic(body["tool_choice"])
    return out


def responses_to_chat_completion(payload: dict[str, Any], model: str) -> dict[str, Any]:
    text = ""
    reasoning = ""
    tool_calls: list[dict[str, Any]] = []
    for item in payload.get("output") or []:
        if item.get("type") == "reasoning":
            for summary in item.get("summary") or []:
                reasoning += summary.get("text", "")
        elif item.get("type") == "message":
            for content in item.get("content") or []:
                if content.get("type") in ("output_text", "text"):
                    text += content.get("text", "")
        elif item.get("type") == "function_call":
            tool_calls.append(
                {
                    "id": item.get("call_id"),
                    "type": "function",
                    "function": {
                        "name": item.get("name"),
                        "arguments": item.get("arguments") or "{}",
                    },
                }
            )
    message: dict[str, Any] = {"role": "assistant", "content": text}
    if reasoning:
        message["reasoning_content"] = reasoning
    if tool_calls:
        message["tool_calls"] = tool_calls
    usage = payload.get("usage") or {}
    prompt = int(usage.get("input_tokens") or 0)
    completion = int(usage.get("output_tokens") or 0)
    return {
        "id": f"chatcmpl_{uuid.uuid4().hex}",
        "object": "chat.completion",
        "created": int(time.time()),
        "model": model,
        "choices": [
            {
                "index": 0,
                "message": message,
                "finish_reason": "tool_calls"
                if tool_calls
                else ("length" if payload.get("status") == "incomplete" else "stop"),
            }
        ],
        "usage": {
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "total_tokens": prompt + completion,
            "prompt_tokens_details": usage.get("input_tokens_details") or {"cached_tokens": 0},
            "completion_tokens_details": usage.get("output_tokens_details")
            or {"reasoning_tokens": 0},
        },
    }


def responses_to_anthropic_message(payload: dict[str, Any], model: str) -> dict[str, Any]:
    content: list[dict[str, Any]] = []
    has_tool_use = False
    for item in payload.get("output") or []:
        if item.get("type") == "reasoning":
            text = "".join(s.get("text", "") for s in item.get("summary") or [])
            if text:
                content.append({"type": "thinking", "thinking": text})
        elif item.get("type") == "message":
            text = ""
            citations: list[dict[str, Any]] = []
            for block in item.get("content") or []:
                if block.get("type") in ("output_text", "text"):
                    text += block.get("text", "")
                    citations.extend(
                        _openai_annotations_to_anthropic(block.get("annotations") or [])
                    )
            if text:
                text_block: dict[str, Any] = {"type": "text", "text": text}
                if citations:
                    text_block["citations"] = citations
                content.append(text_block)
        elif item.get("type") == "function_call":
            has_tool_use = True
            args = item.get("arguments") or "{}"
            try:
                parsed = json.loads(args)
            except Exception:
                parsed = args
            content.append(
                {
                    "type": "tool_use",
                    "id": item.get("call_id"),
                    "name": item.get("name"),
                    "input": parsed,
                }
            )
        elif item.get("type") == "web_search_call":
            action = item.get("action") or {}
            content.append(
                {
                    "type": "server_tool_use",
                    "id": item.get("id"),
                    "name": "web_search",
                    "input": {"query": web_search_query_from_action(action)},
                }
            )
    usage = payload.get("usage") or {}
    message = {
        "id": f"msg_{uuid.uuid4().hex}",
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": "end_turn",
        "stop_sequence": None,
        "usage": {
            "input_tokens": int(usage.get("input_tokens") or 0),
            "output_tokens": int(usage.get("output_tokens") or 0),
        },
    }
    if payload.get("status") == "incomplete":
        message["stop_reason"] = "max_tokens"
    elif has_tool_use:
        message["stop_reason"] = "tool_use"
    return message


@dataclass(slots=True)
class ResponsesDrain:
    text_out: str = ""
    reasoning_out: str = ""
    tool_calls: dict[str, dict[str, str]] = field(default_factory=dict)
    output_items: list[dict[str, Any]] = field(default_factory=list)
    completed_response: dict[str, Any] | None = None
    upstream_error: str | None = None
    status: str = "completed"
    usage: dict[str, Any] = field(default_factory=dict)


def update_drain_from_response_event(
    drain: ResponsesDrain,
    event: str,
    data: dict[str, Any],
) -> None:
    if event in ("response.output_text.delta", "response.refusal.delta"):
        drain.text_out += data.get("delta") or ""
    elif event in ("response.reasoning_summary_text.delta", "response.reasoning_text.delta"):
        drain.reasoning_out += data.get("delta") or ""
    elif event == "response.output_item.done":
        item = data.get("item") or {}
        if not item:
            return
        item_type = item.get("type")
        if item_type == "function_call":
            key = str(item.get("id") or data.get("output_index"))
            drain.tool_calls[key] = {
                "id": str(item.get("call_id") or item.get("id") or ""),
                "name": str(item.get("name") or ""),
                "args": str(item.get("arguments") or ""),
            }
        elif item_type not in ("message", "reasoning"):
            drain.output_items.append(item)
    elif event == "response.output_item.added":
        item = data.get("item") or {}
        if item.get("type") == "function_call":
            key = str(item.get("id") or data.get("output_index"))
            drain.tool_calls[key] = {
                "id": str(item.get("call_id") or item.get("id") or ""),
                "name": str(item.get("name") or ""),
                "args": str(item.get("arguments") or ""),
            }
    elif event == "response.function_call_arguments.delta":
        key = str(data.get("item_id") or data.get("output_index"))
        call = drain.tool_calls.setdefault(key, {"id": "", "name": "", "args": ""})
        call["args"] = f"{call.get('args', '')}{data.get('delta') or ''}"
    elif event == "response.function_call_arguments.done":
        item = data.get("item") or data
        key = str(item.get("id") or data.get("item_id") or data.get("output_index"))
        drain.tool_calls[key] = {
            "id": str(item.get("call_id") or item.get("id") or ""),
            "name": str(item.get("name") or ""),
            "args": str(item.get("arguments") or ""),
        }
    elif event == "response.completed":
        response = data.get("response") or data
        drain.completed_response = response
        drain.status = response.get("status") or "completed"
        drain.usage = response.get("usage") or drain.usage
    elif event == "response.incomplete":
        drain.status = "incomplete"
        response = data.get("response") or data
        drain.usage = response.get("usage") or drain.usage
    elif event == "response.failed":
        error = data.get("error") or {}
        drain.upstream_error = error.get("message") if isinstance(error, dict) else str(error)


def usage_from_responses(usage: dict[str, Any] | None) -> UsageData:
    usage = usage or {}
    return UsageData(
        input_tokens=int(usage.get("input_tokens") or 0),
        output_tokens=int(usage.get("output_tokens") or 0),
        cache_read_input_tokens=int(
            (usage.get("input_tokens_details") or {}).get("cached_tokens") or 0
        ),
        reasoning_output_tokens=int(
            (usage.get("output_tokens_details") or {}).get("reasoning_tokens") or 0
        ),
    )
