from __future__ import annotations

import asyncio
from urllib.parse import parse_qs, urlparse

from helpers import jwt

from pengepul.oauth import (
    CODEX_CALLBACK_PATH,
    CODEX_CALLBACK_PORT,
    CODEX_CLIENT_ID,
    CODEX_TOKEN_URL,
    generate_anthropic_auth_url,
    generate_codex_auth_url,
    refresh_codex_tokens,
)
from pengepul.types import PKCECodes


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


def test_codex_refresh_uses_form_encoded_request(monkeypatch) -> None:
    captured: dict[str, object] = {}

    class FakeResponse:
        status_code = 200
        text = ""

        def json(self) -> dict[str, object]:
            return {
                "access_token": "access-token",
                "refresh_token": "next-refresh-token",
                "expires_in": 3600,
                "id_token": jwt({"email": "bob@example.com"}),
            }

    class FakeClient:
        def __init__(self, **kwargs) -> None:
            captured["timeout"] = kwargs.get("timeout")

        async def __aenter__(self) -> FakeClient:
            return self

        async def __aexit__(self, *_args) -> None:
            return None

        async def post(self, url, *, data=None, json=None, headers=None):
            captured["url"] = url
            captured["data"] = data
            captured["json"] = json
            captured["headers"] = headers
            return FakeResponse()

    monkeypatch.setattr("pengepul.oauth.httpx.AsyncClient", FakeClient)

    token = asyncio.run(refresh_codex_tokens("refresh-token"))

    assert token.email == "bob@example.com"
    assert captured == {
        "timeout": 30,
        "url": CODEX_TOKEN_URL,
        "data": {
            "client_id": CODEX_CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": "refresh-token",
        },
        "json": None,
        "headers": {"Content-Type": "application/x-www-form-urlencoded"},
    }
