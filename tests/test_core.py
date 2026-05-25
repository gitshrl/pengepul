from __future__ import annotations

import asyncio
import base64
import json
from collections.abc import AsyncIterator
from pathlib import Path
from urllib.parse import parse_qs, urlparse

from fastapi.testclient import TestClient

from pengepul.app import create_app
from pengepul.config import Config
from pengepul.oauth import (
    CODEX_CALLBACK_PATH,
    CODEX_CALLBACK_PORT,
    generate_anthropic_auth_url,
    generate_codex_auth_url,
)
from pengepul.providers import build_registry
from pengepul.streaming import (
    AnthropicStreamState,
    iter_sse_events,
    responses_sse_to_anthropic,
)
from pengepul.tokens import load_all_tokens, save_token
from pengepul.translate import (
    anthropic_to_responses_request,
    chat_to_responses_request,
    openai_to_anthropic,
    resolve_model,
)
from pengepul.types import PKCECodes, TokenData


def test_registry_routes_only_anthropic_and_codex(tmp_path: Path) -> None:
    registry = build_registry(str(tmp_path))

    assert [provider.id for provider in registry.all()] == ["anthropic", "codex"]
    assert registry.for_model("claude-sonnet-4-6").id == "anthropic"
    assert registry.for_model("sonnet").id == "anthropic"
    assert registry.for_model("gpt-5").id == "codex"
    assert registry.for_model("gpt-5.4-mini").id == "codex"
    assert registry.for_model("o4-mini").id == "codex"
    assert registry.for_model("codex-mini-latest").id == "codex"
    assert registry.for_model("gpt-4o").id == "anthropic"
    assert registry.for_model("custom-model").id == "anthropic"


def test_token_storage_round_trips_provider_files(tmp_path: Path) -> None:
    save_token(
        str(tmp_path),
        TokenData(
            access_token="claude-access",
            refresh_token="claude-refresh",
            email="alice@example.com",
            expires_at="2030-01-01T00:00:00Z",
            account_uuid="acct-claude",
            provider="anthropic",
        ),
    )
    save_token(
        str(tmp_path),
        TokenData(
            access_token="codex-access",
            refresh_token="codex-refresh",
            email="bob@example.com",
            expires_at="2030-01-01T00:00:00Z",
            account_uuid="acct-codex",
            provider="codex",
            id_token=_jwt({"email": "bob@example.com"}),
        ),
    )

    assert sorted(path.name for path in tmp_path.iterdir() if path.suffix == ".json") == [
        "claude-alice@example.com.json",
        "codex-bob@example.com.json",
    ]
    assert [token.email for token in load_all_tokens(str(tmp_path), "anthropic")] == [
        "alice@example.com"
    ]
    assert [token.email for token in load_all_tokens(str(tmp_path), "codex")] == ["bob@example.com"]


def test_oauth_urls_use_expected_callback_and_scope() -> None:
    pkce = PKCECodes(code_verifier="verifier", code_challenge="challenge")

    anthropic = urlparse(generate_anthropic_auth_url("state", pkce))
    anthropic_query = parse_qs(anthropic.query)
    assert anthropic.netloc == "claude.ai"
    assert anthropic_query["redirect_uri"] == ["http://localhost:54545/callback"]
    assert anthropic_query["scope"] == ["org:create_api_key user:profile user:inference"]

    codex = urlparse(generate_codex_auth_url("state", pkce))
    codex_query = parse_qs(codex.query)
    assert codex.netloc == "auth.openai.com"
    assert codex_query["redirect_uri"] == [
        f"http://localhost:{CODEX_CALLBACK_PORT}{CODEX_CALLBACK_PATH}"
    ]
    assert codex_query["originator"] == ["codex_cli_rs"]
    assert codex_query["code_challenge"] == ["challenge"]


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


def test_resolve_model_aliases() -> None:
    assert resolve_model("opus") == "claude-opus-4-7"
    assert resolve_model("sonnet") == "claude-sonnet-4-6"
    assert resolve_model("haiku") == "claude-haiku-4-5-20251001"
    assert resolve_model("gpt-5.4") == "gpt-5.4"


def test_sse_parser_handles_crlf_and_preserves_data_spacing() -> None:
    events = asyncio.run(
        _collect_sse(
            [
                b"event: delta\r\n",
                b"data:  leading space\r\n\r\n",
                b"data: second\r\n\r\n",
            ]
        )
    )

    assert events == [("delta", " leading space"), ("message", "second")]


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

    payloads = [_sse_payload(chunk) for chunk in chunks]
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


class _FakeSSE:
    def __init__(self, chunks: list[bytes]) -> None:
        self._chunks = chunks

    async def aiter_bytes(self) -> AsyncIterator[bytes]:
        for chunk in self._chunks:
            yield chunk


async def _collect_sse(chunks: list[bytes]) -> list[tuple[str, str]]:
    return [event async for event in iter_sse_events(_FakeSSE(chunks))]


def _sse_payload(chunk: str) -> dict[str, object]:
    data = "\n".join(line[6:] for line in chunk.splitlines() if line.startswith("data: "))
    return json.loads(data)


def _jwt(payload: dict[str, object]) -> str:
    def encode(value: dict[str, object]) -> str:
        raw = json.dumps(value).encode("utf-8")
        return base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")

    return f"{encode({'alg': 'none'})}.{encode(payload)}."
