from __future__ import annotations

import asyncio
import json
from pathlib import Path

from pengepul.accounts import AccountManager, RefreshPolicy
from pengepul.types import TokenData


def test_since_last_refresh_refreshes_legacy_token_without_last_refresh(tmp_path: Path) -> None:
    (tmp_path / "codex-bob_example_com.json").write_text(
        json.dumps(
            {
                "access_token": "old-access",
                "refresh_token": "old-refresh",
                "email": "bob@example.com",
                "type": "codex",
                "expired": "2030-01-01T00:00:00Z",
                "account_uuid": "acct-codex",
            }
        ),
        encoding="utf-8",
    )
    refresh_calls: list[str] = []

    async def refresh(refresh_token: str) -> TokenData:
        refresh_calls.append(refresh_token)
        return TokenData(
            access_token="new-access",
            refresh_token="new-refresh",
            email="bob@example.com",
            expires_at="2030-01-01T00:00:00Z",
            account_uuid="acct-codex",
            provider="codex",
        )

    manager = AccountManager(
        str(tmp_path),
        "codex",
        refresh,
        RefreshPolicy(kind="since-last-refresh", seconds=8 * 24 * 60 * 60),
    )
    manager.load()

    assert asyncio.run(manager.refresh_if_due("bob@example.com")) is True

    assert refresh_calls == ["old-refresh"]
    snapshots = manager.get_snapshots()
    assert snapshots[0]["lastRefreshAt"] is not None
