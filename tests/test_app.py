from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

from fastapi.testclient import TestClient
from pytest import MonkeyPatch

from pengepul.app import _response_payload_from_drain, create_app
from pengepul.config import Config
from pengepul.providers import build_registry
from pengepul.tokens import save_token
from pengepul.translate import ResponsesDrain, update_drain_from_response_event
from pengepul.types import TokenData


def test_app_auth_and_no_account_responses(tmp_path: Path) -> None:
    config = Config(auth_dir=str(tmp_path), api_keys={"sk-test"})
    app = create_app(config, build_registry(str(tmp_path)))
    client = TestClient(app)

    assert client.get("/health").json() == {"status": "ok"}
    missing_auth = client.post("/v1/messages", json={})
    assert missing_auth.status_code == 401
    assert missing_auth.json()["error"]["message"] == "missing API key"

    wrong_auth = client.post(
        "/v1/messages",
        headers={"Authorization": "Bearer wrong"},
        json={"model": "sonnet", "messages": [{"role": "user", "content": "hi"}]},
    )
    assert wrong_auth.status_code == 403
    assert wrong_auth.json()["error"]["message"] == "invalid API key"

    response = client.post(
        "/v1/messages",
        headers={"Authorization": "Bearer sk-test"},
        json={"model": "sonnet", "messages": [{"role": "user", "content": "hi"}]},
    )
    assert response.status_code == 503
    assert response.json()["error"]["type"] == "no_account_for_provider"

    codex_count = client.post(
        "/v1/messages/count_tokens",
        headers={"Authorization": "Bearer sk-test"},
        json={"model": "gpt-5.4", "messages": [{"role": "user", "content": "hi"}]},
    )
    assert codex_count.status_code == 501
    assert codex_count.json()["error"]["provider"] == "codex"


def test_app_enforces_configured_body_limit(tmp_path: Path) -> None:
    config = Config(auth_dir=str(tmp_path), api_keys={"sk-test"}, body_limit="10b")
    app = create_app(config, build_registry(str(tmp_path)))
    client = TestClient(app)

    response = client.post(
        "/v1/messages",
        headers={"Authorization": "Bearer sk-test"},
        json={"model": "sonnet", "messages": [{"role": "user", "content": "hi"}]},
    )
    assert response.status_code == 413
    assert response.json()["error"]["message"] == "request body too large"


def test_app_rejects_body_without_content_length_when_limit_configured(tmp_path: Path) -> None:
    config = Config(auth_dir=str(tmp_path), api_keys={"sk-test"}, body_limit="10b")
    app = create_app(config, build_registry(str(tmp_path)))

    status, body = _post_without_content_length(
        app,
        "/v1/messages",
        b'{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}',
    )

    assert status == 411
    assert body == b'{"error":{"message":"missing content-length"}}'


def test_invalid_json_body_returns_400(tmp_path: Path) -> None:
    config = Config(auth_dir=str(tmp_path), api_keys={"sk-test"})
    app = create_app(config, build_registry(str(tmp_path)))
    client = TestClient(app, raise_server_exceptions=False)

    response = client.post(
        "/v1/messages",
        headers={"Authorization": "Bearer sk-test", "Content-Type": "application/json"},
        content=b'{"model":"sonnet","messages":[{"role":"user","content":"bad\njson"}]}',
    )

    assert response.status_code == 400
    assert response.json()["error"]["message"] == "invalid JSON body"


def test_cors_allows_remote_origins(tmp_path: Path) -> None:
    config = Config(auth_dir=str(tmp_path), api_keys={"sk-test"})
    app = create_app(config, build_registry(str(tmp_path)))
    client = TestClient(app)

    response = client.options(
        "/v1/messages",
        headers={
            "Origin": "https://client.example.com",
            "Access-Control-Request-Method": "POST",
            "Access-Control-Request-Headers": "authorization,content-type",
        },
    )

    assert response.status_code == 200
    assert response.headers["access-control-allow-origin"] == "*"


def test_messages_route_resolves_anthropic_model_alias(
    tmp_path: Path,
    monkeypatch: MonkeyPatch,
) -> None:
    _save_provider_token(tmp_path, "anthropic")
    captured: dict[str, Any] = {}

    async def fake_call_anthropic_messages(**kwargs):
        captured["body"] = kwargs["body"]
        return _FakeJsonUpstream(
            {
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-6",
                "content": [{"type": "text", "text": "pong"}],
                "usage": {"input_tokens": 1, "output_tokens": 1},
            }
        )

    monkeypatch.setattr("pengepul.app.call_anthropic_messages", fake_call_anthropic_messages)
    client = TestClient(create_app(Config(auth_dir=str(tmp_path), api_keys={"sk-test"})))

    response = client.post(
        "/v1/messages",
        headers={"Authorization": "Bearer sk-test"},
        json={
            "model": "sonnet",
            "max_tokens": 32,
            "messages": [{"role": "user", "content": "reply exactly: pong"}],
        },
    )

    assert response.status_code == 200
    assert captured["body"]["model"] == "claude-sonnet-4-6"


def test_count_tokens_route_resolves_anthropic_model_alias(
    tmp_path: Path,
    monkeypatch: MonkeyPatch,
) -> None:
    _save_provider_token(tmp_path, "anthropic")
    captured: dict[str, Any] = {}

    async def fake_call_anthropic_count_tokens(**kwargs):
        captured["body"] = kwargs["body"]
        return _FakeJsonUpstream({"input_tokens": 2})

    monkeypatch.setattr(
        "pengepul.app.call_anthropic_count_tokens",
        fake_call_anthropic_count_tokens,
    )
    client = TestClient(create_app(Config(auth_dir=str(tmp_path), api_keys={"sk-test"})))

    response = client.post(
        "/v1/messages/count_tokens",
        headers={"Authorization": "Bearer sk-test"},
        json={
            "model": "sonnet",
            "messages": [{"role": "user", "content": "reply exactly: pong"}],
        },
    )

    assert response.status_code == 200
    assert captured["body"]["model"] == "claude-sonnet-4-6"


def test_messages_route_injects_pi_web_search_for_anthropic(
    tmp_path: Path,
    monkeypatch: MonkeyPatch,
) -> None:
    _save_provider_token(tmp_path, "anthropic")
    captured: dict[str, Any] = {}

    async def fake_call_anthropic_messages(**kwargs):
        captured["body"] = kwargs["body"]
        return _FakeJsonUpstream(
            {
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-6",
                "content": [{"type": "text", "text": "ok"}],
                "usage": {"input_tokens": 1, "output_tokens": 1},
            }
        )

    monkeypatch.setattr("pengepul.app.call_anthropic_messages", fake_call_anthropic_messages)
    client = TestClient(create_app(Config(auth_dir=str(tmp_path), api_keys={"sk-test"})))

    response = client.post(
        "/v1/messages",
        headers={
            "Authorization": "Bearer sk-test",
            "X-Pengepul-Web-Search": "auto",
        },
        json={
            "model": "sonnet",
            "messages": [{"role": "user", "content": "latest docs?"}],
            "tools": [{"name": "get_weather", "input_schema": {"type": "object"}}],
        },
    )

    assert response.status_code == 200
    assert captured["body"]["tools"] == [
        {"name": "get_weather", "input_schema": {"type": "object"}},
        {"type": "web_search_20260209", "name": "web_search"},
    ]


def test_responses_route_sends_web_search_and_reasoning_to_anthropic(
    tmp_path: Path,
    monkeypatch: MonkeyPatch,
) -> None:
    _save_provider_token(tmp_path, "anthropic")
    captured: dict[str, Any] = {}

    async def fake_call_anthropic_messages(**kwargs):
        captured["body"] = kwargs["body"]
        return _FakeJsonUpstream(
            {
                "id": "msg_1",
                "content": [{"type": "text", "text": "ok"}],
                "usage": {"input_tokens": 1, "output_tokens": 1},
            }
        )

    monkeypatch.setattr("pengepul.app.call_anthropic_messages", fake_call_anthropic_messages)
    client = TestClient(create_app(Config(auth_dir=str(tmp_path), api_keys={"sk-test"})))

    response = client.post(
        "/v1/responses",
        headers={"Authorization": "Bearer sk-test"},
        json={
            "model": "sonnet",
            "input": "latest docs?",
            "tools": [{"type": "web_search"}],
            "reasoning": {"effort": "low"},
        },
    )

    assert response.status_code == 200
    assert captured["body"]["tools"] == [{"type": "web_search_20260209", "name": "web_search"}]
    assert captured["body"]["thinking"]["budget_tokens"] == 4096


def test_chat_route_preserves_responses_web_search_for_codex(
    tmp_path: Path,
    monkeypatch: MonkeyPatch,
) -> None:
    _save_provider_token(tmp_path, "codex")
    captured: dict[str, Any] = {}

    async def fake_call_codex_responses(**kwargs):
        captured["body"] = kwargs["body"]
        return _FakeSseUpstream(_completed_response_sse("gpt-5.4"))

    monkeypatch.setattr("pengepul.app.call_codex_responses", fake_call_codex_responses)
    client = TestClient(create_app(Config(auth_dir=str(tmp_path), api_keys={"sk-test"})))

    response = client.post(
        "/v1/chat/completions",
        headers={"Authorization": "Bearer sk-test"},
        json={
            "model": "gpt-5.4",
            "messages": [{"role": "user", "content": "latest docs?"}],
            "responses_tools": [{"type": "web_search", "search_context_size": "low"}],
            "responses_tool_choice": "auto",
        },
    )

    assert response.status_code == 200
    assert captured["body"]["tools"] == [{"type": "web_search", "search_context_size": "low"}]
    assert captured["body"]["tool_choice"] == "auto"
    assert captured["body"]["stream"] is True


def test_responses_route_injects_pi_web_search_for_codex(
    tmp_path: Path,
    monkeypatch: MonkeyPatch,
) -> None:
    _save_provider_token(tmp_path, "codex")
    captured: dict[str, Any] = {}

    async def fake_call_codex_responses(**kwargs):
        captured["body"] = kwargs["body"]
        return _FakeSseUpstream(_completed_response_sse("gpt-5.4"))

    monkeypatch.setattr("pengepul.app.call_codex_responses", fake_call_codex_responses)
    client = TestClient(create_app(Config(auth_dir=str(tmp_path), api_keys={"sk-test"})))

    response = client.post(
        "/v1/responses",
        headers={
            "Authorization": "Bearer sk-test",
            "X-Pengepul-Web-Search": "auto",
        },
        json={"model": "gpt-5.4", "input": "jadwal final ucl kapan?"},
    )

    assert response.status_code == 200
    assert captured["body"]["tools"] == [{"type": "web_search"}]
    assert captured["body"]["stream"] is True


def test_responses_route_does_not_duplicate_pi_web_search_for_codex(
    tmp_path: Path,
    monkeypatch: MonkeyPatch,
) -> None:
    _save_provider_token(tmp_path, "codex")
    captured: dict[str, Any] = {}

    async def fake_call_codex_responses(**kwargs):
        captured["body"] = kwargs["body"]
        return _FakeSseUpstream(_completed_response_sse("gpt-5.4"))

    monkeypatch.setattr("pengepul.app.call_codex_responses", fake_call_codex_responses)
    client = TestClient(create_app(Config(auth_dir=str(tmp_path), api_keys={"sk-test"})))

    response = client.post(
        "/v1/responses",
        headers={
            "Authorization": "Bearer sk-test",
            "X-Pengepul-Web-Search": "auto",
        },
        json={
            "model": "gpt-5.4",
            "input": "jadwal final ucl kapan?",
            "tools": [{"type": "web_search", "search_context_size": "low"}],
        },
    )

    assert response.status_code == 200
    assert captured["body"]["tools"] == [{"type": "web_search", "search_context_size": "low"}]


def test_responses_route_normalizes_string_input_for_codex(
    tmp_path: Path,
    monkeypatch: MonkeyPatch,
) -> None:
    _save_provider_token(tmp_path, "codex")
    captured: dict[str, Any] = {}

    async def fake_call_codex_responses(**kwargs):
        captured["body"] = kwargs["body"]
        return _FakeSseUpstream(_completed_response_sse("gpt-5.4"))

    monkeypatch.setattr("pengepul.app.call_codex_responses", fake_call_codex_responses)
    client = TestClient(create_app(Config(auth_dir=str(tmp_path), api_keys={"sk-test"})))

    response = client.post(
        "/v1/responses",
        headers={"Authorization": "Bearer sk-test"},
        json={
            "model": "gpt-5.4",
            "input": "reply exactly: pong",
            "max_output_tokens": 32,
        },
    )

    assert response.status_code == 200
    assert captured["body"]["input"] == [{"role": "user", "content": "reply exactly: pong"}]


def test_messages_route_translates_anthropic_web_search_for_codex(
    tmp_path: Path,
    monkeypatch: MonkeyPatch,
) -> None:
    _save_provider_token(tmp_path, "codex")
    captured: dict[str, Any] = {}

    async def fake_call_codex_responses(**kwargs):
        captured["body"] = kwargs["body"]
        return _FakeSseUpstream(_completed_response_sse("gpt-5.4"))

    monkeypatch.setattr("pengepul.app.call_codex_responses", fake_call_codex_responses)
    client = TestClient(create_app(Config(auth_dir=str(tmp_path), api_keys={"sk-test"})))

    response = client.post(
        "/v1/messages",
        headers={"Authorization": "Bearer sk-test"},
        json={
            "model": "gpt-5.4",
            "messages": [{"role": "user", "content": "latest docs?"}],
            "tools": [
                {
                    "type": "web_search_20250305",
                    "name": "web_search",
                    "allowed_domains": ["docs.anthropic.com"],
                }
            ],
        },
    )

    assert response.status_code == 200
    assert captured["body"]["tools"] == [
        {"type": "web_search", "filters": {"allowed_domains": ["docs.anthropic.com"]}}
    ]
    assert captured["body"]["stream"] is True


def test_messages_route_forwards_anthropic_tool_choice_for_codex(
    tmp_path: Path,
    monkeypatch: MonkeyPatch,
) -> None:
    _save_provider_token(tmp_path, "codex")
    captured: dict[str, Any] = {}

    async def fake_call_codex_responses(**kwargs):
        captured["body"] = kwargs["body"]
        return _FakeSseUpstream(_completed_response_sse("gpt-5.4"))

    monkeypatch.setattr("pengepul.app.call_codex_responses", fake_call_codex_responses)
    client = TestClient(create_app(Config(auth_dir=str(tmp_path), api_keys={"sk-test"})))

    response = client.post(
        "/v1/messages",
        headers={"Authorization": "Bearer sk-test"},
        json={
            "model": "gpt-5.4",
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
        },
    )

    assert response.status_code == 200
    assert captured["body"]["tool_choice"] == {"type": "function", "name": "get_weather"}
    assert captured["body"]["stream"] is True


def test_response_payload_from_drain_preserves_hosted_tool_and_text() -> None:
    web_search_call = {
        "type": "web_search_call",
        "id": "ws_1",
        "status": "completed",
        "action": {"type": "search", "query": "latest docs"},
    }
    drain = ResponsesDrain(
        text_out="found it",
        output_items=[web_search_call],
        usage={"input_tokens": 1, "output_tokens": 2},
    )

    payload = _response_payload_from_drain(drain, "gpt-5.4")

    assert payload["output"] == [
        web_search_call,
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "found it"}],
        },
    ]
    assert payload["output_text"] == "found it"


def test_response_payload_from_drain_uses_done_function_call_arguments() -> None:
    drain = ResponsesDrain()
    update_drain_from_response_event(
        drain,
        "response.output_item.done",
        {
            "item": {
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": '{"city":"SF"}',
            }
        },
    )

    payload = _response_payload_from_drain(drain, "gpt-5.4")

    assert payload["output"] == [
        {
            "type": "function_call",
            "call_id": "call_1",
            "name": "get_weather",
            "arguments": '{"city":"SF"}',
        }
    ]


class _FakeJsonUpstream:
    status_code = 200

    def __init__(self, payload: dict[str, Any]) -> None:
        self._payload = payload

    async def json(self) -> dict[str, Any]:
        return self._payload

    async def text(self) -> str:
        return ""

    async def aclose(self) -> None:
        return None


class _FakeSseUpstream:
    status_code = 200

    def __init__(self, chunks: list[bytes]) -> None:
        self._chunks = chunks

    async def aiter_bytes(self):
        for chunk in self._chunks:
            yield chunk

    async def text(self) -> str:
        return ""

    async def aclose(self) -> None:
        return None


def _save_provider_token(tmp_path: Path, provider: str) -> None:
    save_token(
        str(tmp_path),
        TokenData(
            access_token=f"{provider}-access",
            refresh_token=f"{provider}-refresh",
            email=f"{provider}@example.com",
            expires_at="2030-01-01T00:00:00Z",
            account_uuid=f"acct-{provider}",
            provider=provider,
        ),
    )


def _completed_response_sse(model: str) -> list[bytes]:
    return [
        (
            "event: response.completed\n"
            'data: {"response":{"id":"resp_1","object":"response","status":"completed",'
            f'"model":"{model}","output":[{{"type":"message","role":"assistant",'
            '"content":[{"type":"output_text","text":"ok"}]}],"usage":{"input_tokens":1,'
            '"output_tokens":1,"total_tokens":2}}}\n\n'
        ).encode()
    ]


def _post_without_content_length(app, path: str, body: bytes) -> tuple[int, bytes]:
    messages: list[dict[str, Any]] = []

    scope = {
        "type": "http",
        "asgi": {"version": "3.0"},
        "http_version": "1.1",
        "method": "POST",
        "scheme": "http",
        "path": path,
        "raw_path": path.encode("ascii"),
        "query_string": b"",
        "headers": [
            (b"authorization", b"Bearer sk-test"),
            (b"content-type", b"application/json"),
        ],
        "client": ("testclient", 50000),
        "server": ("testserver", 80),
    }
    sent = False

    async def receive() -> dict[str, object]:
        nonlocal sent
        if sent:
            return {"type": "http.disconnect"}
        sent = True
        return {"type": "http.request", "body": body, "more_body": False}

    async def send(message: dict[str, Any]) -> None:
        messages.append(message)

    async def call() -> None:
        await app(scope, receive, send)

    asyncio.run(call())
    status = next(
        message["status"] for message in messages if message["type"] == "http.response.start"
    )
    response_body = b"".join(
        message.get("body", b"") for message in messages if message["type"] == "http.response.body"
    )
    return int(status), response_body
