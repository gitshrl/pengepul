from __future__ import annotations

import time
from collections import defaultdict
from json import JSONDecodeError
from typing import Any

from fastapi import Depends, FastAPI, Header, HTTPException, Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse, Response, StreamingResponse

from .config import Config
from .providers import ProviderRegistry, build_registry
from .proxy import openai_error_body, proxy_with_retry
from .streaming import (
    AnthropicStreamState,
    ChatStreamState,
    ResponsesStreamState,
    anthropic_sse_to_chat,
    anthropic_sse_to_responses,
    drain_responses_sse,
    passthrough_sse,
    responses_sse_to_anthropic,
    responses_sse_to_chat,
    transformed_sse,
)
from .translate import (
    anthropic_to_openai,
    anthropic_to_responses,
    anthropic_to_responses_request,
    chat_to_responses_request,
    openai_to_anthropic,
    resolve_model,
    responses_to_anthropic,
    responses_to_anthropic_message,
    responses_to_chat_completion,
    usage_from_responses,
)
from .types import AvailableAccount, extract_usage
from .upstream import (
    apply_cloaking,
    call_anthropic_count_tokens,
    call_anthropic_messages,
    call_codex_responses,
    normalize_codex_responses_body,
)
from .utils import extract_api_key, now_iso

RATE_LIMIT_WINDOW_SECONDS = 60
RATE_LIMIT_MAX = 60


def create_app(config: Config, registry: ProviderRegistry | None = None) -> FastAPI:
    registry = registry or build_registry(config.auth_dir)
    for provider in registry.all():
        if provider.manager.account_count == 0:
            provider.manager.load()

    app = FastAPI(title="pengepul")
    buckets: dict[str, tuple[int, float]] = defaultdict(lambda: (0, 0.0))
    body_limit_bytes = _parse_body_limit(config.body_limit)

    app.add_middleware(
        CORSMiddleware,
        allow_origins=["*"],
        allow_methods=["GET", "POST", "OPTIONS"],
        allow_headers=["*"],
    )

    @app.exception_handler(HTTPException)
    async def http_exception_handler(_request: Request, exc: HTTPException) -> JSONResponse:
        detail = exc.detail
        message = detail.get("message") if isinstance(detail, dict) else str(detail)
        return JSONResponse({"error": {"message": message}}, status_code=exc.status_code)

    @app.exception_handler(JSONDecodeError)
    async def json_decode_error_handler(_request: Request, _exc: JSONDecodeError) -> JSONResponse:
        return JSONResponse({"error": {"message": "invalid JSON body"}}, status_code=400)

    @app.middleware("http")
    async def enforce_body_limit(request: Request, call_next):
        if request.method in {"POST", "PUT", "PATCH"} and body_limit_bytes is not None:
            content_length = request.headers.get("content-length")
            if content_length is None:
                return JSONResponse(
                    {"error": {"message": "missing content-length"}},
                    status_code=411,
                )
            try:
                declared_length = int(content_length)
            except ValueError:
                return JSONResponse(
                    {"error": {"message": "invalid content-length"}},
                    status_code=400,
                )
            if declared_length > body_limit_bytes:
                return JSONResponse(
                    {"error": {"message": "request body too large"}},
                    status_code=413,
                )
        return await call_next(request)

    async def require_api_key(
        request: Request,
        authorization: str | None = Header(default=None),
        x_api_key: str | None = Header(default=None),
    ) -> str:
        api_key = extract_api_key({"authorization": authorization, "x-api-key": x_api_key})
        if not api_key:
            raise HTTPException(status_code=401, detail={"message": "missing API key"})
        if api_key not in config.api_keys:
            raise HTTPException(status_code=403, detail={"message": "invalid API key"})
        if request.url.path.startswith("/v1/"):
            client = request.client.host if request.client else "unknown"
            count, reset_at = buckets[client]
            now = time.time()
            if now > reset_at:
                buckets[client] = (1, now + RATE_LIMIT_WINDOW_SECONDS)
            else:
                buckets[client] = (count + 1, reset_at)
                if count + 1 > RATE_LIMIT_MAX:
                    raise HTTPException(status_code=429, detail={"message": "too many requests"})
        return api_key

    @app.get("/health")
    async def health() -> dict[str, str]:
        return {"status": "ok"}

    @app.get("/admin/accounts", dependencies=[Depends(require_api_key)])
    async def admin_accounts() -> dict[str, Any]:
        return {
            "providers": {
                provider.id: {
                    "accounts": provider.manager.get_snapshots(),
                    "account_count": provider.manager.account_count,
                }
                for provider in registry.all()
            },
            "generated_at": now_iso(),
        }

    @app.post("/admin/reload", dependencies=[Depends(require_api_key)])
    async def admin_reload() -> dict[str, Any]:
        return {
            "reloaded": {provider.id: provider.manager.reload() for provider in registry.all()},
            "generated_at": now_iso(),
        }

    @app.get("/v1/models", dependencies=[Depends(require_api_key)])
    async def models() -> dict[str, Any]:
        created = int(time.time())
        data: list[dict[str, Any]] = []
        for provider in registry.with_accounts():
            for model in await provider.list_models():
                data.append(
                    {
                        "id": model["id"],
                        "object": "model",
                        "created": created,
                        "owned_by": model["owned_by"],
                    }
                )
        return {"object": "list", "data": data}

    @app.post("/v1/chat/completions", dependencies=[Depends(require_api_key)])
    async def chat_completions(request: Request) -> Response:
        body = await request.json()
        if not isinstance(body.get("messages"), list) or not body["messages"]:
            return JSONResponse(
                {"error": {"message": "messages is required and must be a non-empty array"}},
                status_code=400,
            )
        model = resolve_model(body.get("model"))
        provider = registry.for_model(model)
        if provider.id == "codex":
            return await _codex_chat(request, config, provider.manager, body, model)
        return await _anthropic_chat(request, config, provider.manager, body, model)

    @app.post("/v1/responses", dependencies=[Depends(require_api_key)])
    async def responses(request: Request) -> Response:
        body = await request.json()
        if "input" not in body and "messages" not in body:
            return JSONResponse({"error": {"message": "input is required"}}, status_code=400)
        model = resolve_model(body.get("model"))
        provider = registry.for_model(model)
        if provider.id == "codex":
            return await _codex_responses(config, provider.manager, body, model)
        return await _anthropic_responses(request, config, provider.manager, body, model)

    @app.post("/v1/messages", dependencies=[Depends(require_api_key)])
    async def messages(request: Request) -> Response:
        body = await request.json()
        if not isinstance(body.get("messages"), list) or not body["messages"]:
            return JSONResponse(
                {"error": {"message": "messages is required and must be a non-empty array"}},
                status_code=400,
            )
        model = resolve_model(body.get("model"))
        provider = registry.for_model(model)
        if provider.id == "codex":
            return await _codex_messages(request, config, provider.manager, body, model)
        return await _anthropic_messages(request, config, provider.manager, body, model)

    @app.post("/v1/messages/count_tokens", dependencies=[Depends(require_api_key)])
    async def count_tokens(request: Request) -> Response:
        body = await request.json()
        model = resolve_model(body.get("model"))
        provider = registry.for_model(model)
        if provider.id == "codex":
            return JSONResponse(
                {
                    "error": {
                        "message": "count_tokens is not supported for the codex provider",
                        "type": "unsupported_endpoint_for_provider",
                        "provider": "codex",
                    }
                },
                status_code=501,
            )
        body = {**body, "model": model}

        async def upstream(account: AvailableAccount):
            return await call_anthropic_count_tokens(
                body=body,
                request_headers=_headers(request),
                account=account,
                config=config,
            )

        async def success(upstream_response, account: AvailableAccount):
            try:
                payload = await upstream_response.json()
                provider.manager.record_success(account.token.email)
                return JSONResponse(payload)
            finally:
                await upstream_response.aclose()

        return await proxy_with_retry(provider.manager, upstream, success)

    return app


async def _anthropic_chat(
    request: Request,
    config: Config,
    manager,
    body: dict[str, Any],
    model: str,
) -> Response:
    stream = bool(body.get("stream"))
    translated = openai_to_anthropic(body)
    structured = (body.get("response_format") or {}).get("type") in ("json_object", "json_schema")

    async def upstream(account: AvailableAccount):
        cloaked = apply_cloaking(
            translated,
            request_headers=_headers(request),
            account=account,
            config=config,
        )
        return await call_anthropic_messages(
            body=cloaked,
            request_headers=_headers(request),
            account=account,
            config=config,
            structured=structured,
        )

    async def success(upstream_response, account: AvailableAccount):
        if stream:
            state = ChatStreamState(
                model=model,
                include_usage=(body.get("stream_options") or {}).get("include_usage") is not False,
            )
            return StreamingResponse(
                transformed_sse(
                    upstream_response,
                    lambda event, data, usage: anthropic_sse_to_chat(event, data, state, usage),
                    lambda usage: manager.record_success(account.token.email, usage),
                    lambda detail: manager.record_failure(account.token.email, "network", detail),
                ),
                media_type="text/event-stream",
            )
        try:
            payload = await upstream_response.json()
            usage = extract_usage(payload)
            manager.record_success(account.token.email, usage)
            return JSONResponse(anthropic_to_openai(payload, model))
        finally:
            await upstream_response.aclose()

    return await proxy_with_retry(manager, upstream, success, openai_error_body)


async def _codex_chat(
    request: Request,
    config: Config,
    manager,
    body: dict[str, Any],
    model: str,
) -> Response:
    client_wants_stream = bool(body.get("stream"))
    responses_body = normalize_codex_responses_body(chat_to_responses_request(body))
    responses_body.pop("max_output_tokens", None)
    responses_body.pop("parallel_tool_calls", None)
    responses_body["stream"] = True

    async def upstream(account: AvailableAccount):
        return await call_codex_responses(body=responses_body, account=account, config=config)

    async def success(upstream_response, account: AvailableAccount):
        if client_wants_stream:
            state = ChatStreamState(model=model)
            return StreamingResponse(
                transformed_sse(
                    upstream_response,
                    lambda event, data, _usage: responses_sse_to_chat(event, data, state),
                    lambda usage: manager.record_success(account.token.email, usage),
                    lambda detail: manager.record_failure(account.token.email, "network", detail),
                ),
                media_type="text/event-stream",
            )
        drain = await drain_responses_sse(upstream_response)
        if drain.upstream_error and not (drain.text_out or drain.reasoning_out or drain.tool_calls):
            manager.record_failure(account.token.email, "server", drain.upstream_error)
            return JSONResponse(
                {"error": {"message": drain.upstream_error, "type": "upstream_error"}},
                status_code=502,
            )
        response_payload = _response_payload_from_drain(drain, model)
        usage = usage_from_responses(response_payload.get("usage"))
        manager.record_success(account.token.email, usage)
        return JSONResponse(responses_to_chat_completion(response_payload, model))

    return await proxy_with_retry(manager, upstream, success, openai_error_body)


async def _anthropic_responses(
    request: Request,
    config: Config,
    manager,
    body: dict[str, Any],
    model: str,
) -> Response:
    client_wants_stream = bool(body.get("stream"))
    translated = responses_to_anthropic(body)
    structured = ((body.get("text") or {}).get("format") or {}).get("type") in (
        "json_object",
        "json_schema",
    )

    async def upstream(account: AvailableAccount):
        cloaked = apply_cloaking(
            translated,
            request_headers=_headers(request),
            account=account,
            config=config,
        )
        return await call_anthropic_messages(
            body=cloaked,
            request_headers=_headers(request),
            account=account,
            config=config,
            structured=structured,
        )

    async def success(upstream_response, account: AvailableAccount):
        if client_wants_stream:
            state = ResponsesStreamState(model=model)
            return StreamingResponse(
                transformed_sse(
                    upstream_response,
                    lambda event, data, usage: anthropic_sse_to_responses(
                        event, data, state, model, usage
                    ),
                    lambda usage: manager.record_success(account.token.email, usage),
                    lambda detail: manager.record_failure(account.token.email, "network", detail),
                ),
                media_type="text/event-stream",
            )
        try:
            payload = await upstream_response.json()
            usage = extract_usage(payload)
            manager.record_success(account.token.email, usage)
            return JSONResponse(anthropic_to_responses(payload, model))
        finally:
            await upstream_response.aclose()

    return await proxy_with_retry(manager, upstream, success, openai_error_body)


async def _codex_responses(config: Config, manager, body: dict[str, Any], model: str) -> Response:
    client_wants_stream = bool(body.get("stream"))
    responses_body = normalize_codex_responses_body(body)
    responses_body.pop("max_output_tokens", None)
    responses_body.pop("parallel_tool_calls", None)
    responses_body["stream"] = True

    async def upstream(account: AvailableAccount):
        return await call_codex_responses(body=responses_body, account=account, config=config)

    async def success(upstream_response, account: AvailableAccount):
        if client_wants_stream:
            return StreamingResponse(
                passthrough_sse(
                    upstream_response,
                    lambda usage: manager.record_success(account.token.email, usage),
                    lambda detail: manager.record_failure(account.token.email, "network", detail),
                ),
                media_type="text/event-stream",
            )
        drain = await drain_responses_sse(upstream_response)
        if drain.upstream_error and not drain.completed_response:
            manager.record_failure(account.token.email, "server", drain.upstream_error)
            return JSONResponse(
                {"error": {"message": drain.upstream_error, "type": "upstream_error"}},
                status_code=502,
            )
        response_payload = _response_payload_from_drain(drain, model)
        usage = usage_from_responses(response_payload.get("usage"))
        manager.record_success(account.token.email, usage)
        return JSONResponse(response_payload)

    return await proxy_with_retry(manager, upstream, success, openai_error_body)


async def _anthropic_messages(
    request: Request,
    config: Config,
    manager,
    body: dict[str, Any],
    model: str,
) -> Response:
    stream = bool(body.get("stream"))
    resolved_body = {**body, "model": model}

    async def upstream(account: AvailableAccount):
        cloaked = apply_cloaking(
            resolved_body,
            request_headers=_headers(request),
            account=account,
            config=config,
        )
        return await call_anthropic_messages(
            body=cloaked,
            request_headers=_headers(request),
            account=account,
            config=config,
        )

    async def success(upstream_response, account: AvailableAccount):
        if stream:
            return StreamingResponse(
                passthrough_sse(
                    upstream_response,
                    lambda usage: manager.record_success(account.token.email, usage),
                    lambda detail: manager.record_failure(account.token.email, "network", detail),
                ),
                media_type="text/event-stream",
            )
        try:
            payload = await upstream_response.json()
            usage = extract_usage(payload)
            manager.record_success(account.token.email, usage)
            return JSONResponse(payload)
        finally:
            await upstream_response.aclose()

    return await proxy_with_retry(manager, upstream, success)


async def _codex_messages(
    request: Request,
    config: Config,
    manager,
    body: dict[str, Any],
    model: str,
) -> Response:
    client_wants_stream = bool(body.get("stream"))
    responses_body = normalize_codex_responses_body(anthropic_to_responses_request(body))
    responses_body.pop("max_output_tokens", None)
    responses_body.pop("parallel_tool_calls", None)
    responses_body["stream"] = True

    async def upstream(account: AvailableAccount):
        return await call_codex_responses(body=responses_body, account=account, config=config)

    async def success(upstream_response, account: AvailableAccount):
        if client_wants_stream:
            state = AnthropicStreamState(model=model)
            return StreamingResponse(
                transformed_sse(
                    upstream_response,
                    lambda event, data, _usage: responses_sse_to_anthropic(event, data, state),
                    lambda usage: manager.record_success(account.token.email, usage),
                    lambda detail: manager.record_failure(account.token.email, "network", detail),
                ),
                media_type="text/event-stream",
            )
        drain = await drain_responses_sse(upstream_response)
        if drain.upstream_error and not (drain.text_out or drain.reasoning_out or drain.tool_calls):
            manager.record_failure(account.token.email, "server", drain.upstream_error)
            return JSONResponse(
                {"error": {"message": drain.upstream_error, "type": "upstream_error"}},
                status_code=502,
            )
        response_payload = _response_payload_from_drain(drain, model)
        usage = usage_from_responses(response_payload.get("usage"))
        manager.record_success(account.token.email, usage)
        return JSONResponse(responses_to_anthropic_message(response_payload, model))

    return await proxy_with_retry(manager, upstream, success)


def _headers(request: Request) -> dict[str, str]:
    return {key.lower(): value for key, value in request.headers.items()}


def _parse_body_limit(value: str) -> int | None:
    raw = value.strip().lower()
    if not raw:
        return None
    units = {
        "b": 1,
        "kb": 1024,
        "mb": 1024 * 1024,
        "gb": 1024 * 1024 * 1024,
    }
    for suffix, multiplier in sorted(units.items(), key=lambda item: -len(item[0])):
        if raw.endswith(suffix):
            return int(float(raw[: -len(suffix)].strip()) * multiplier)
    return int(raw)


def _response_payload_from_drain(drain, model: str) -> dict[str, Any]:
    if drain.completed_response:
        payload = dict(drain.completed_response)
        if not payload.get("output"):
            payload["output"] = _output_from_drain(drain)
        payload.setdefault("output_text", drain.text_out)
        return payload
    return {
        "id": f"resp_{int(time.time() * 1000)}",
        "object": "response",
        "created_at": int(time.time()),
        "status": drain.status,
        "model": model,
        "output": _output_from_drain(drain),
        "output_text": drain.text_out,
        "usage": drain.usage,
    }


def _output_from_drain(drain) -> list[dict[str, Any]]:
    output: list[dict[str, Any]] = list(drain.output_items)
    if drain.reasoning_out:
        output.append(
            {
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": drain.reasoning_out}],
            }
        )
    if drain.text_out:
        output.append(
            {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": drain.text_out}],
            }
        )
    for call in drain.tool_calls.values():
        output.append(
            {
                "type": "function_call",
                "call_id": call.get("id"),
                "name": call.get("name"),
                "arguments": call.get("args") or "{}",
            }
        )
    return output
