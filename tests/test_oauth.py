from __future__ import annotations

from urllib.parse import parse_qs, urlparse

from pengepul.oauth import (
    CODEX_CALLBACK_PATH,
    CODEX_CALLBACK_PORT,
    generate_anthropic_auth_url,
    generate_codex_auth_url,
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
