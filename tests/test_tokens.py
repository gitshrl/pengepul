from __future__ import annotations

from pathlib import Path

from helpers import jwt

from pengepul.tokens import load_all_tokens, save_token
from pengepul.types import TokenData


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
            id_token=jwt({"email": "bob@example.com"}),
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
