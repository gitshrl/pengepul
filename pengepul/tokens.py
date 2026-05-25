from __future__ import annotations

import json
import logging
import os
from pathlib import Path
from typing import Any

from .types import ProviderId, TokenData
from .utils import decode_jwt_payload, now_iso, sanitize_email

logger = logging.getLogger(__name__)

PREFIX_BY_PROVIDER: dict[ProviderId, str] = {
    "anthropic": "claude",
    "codex": "codex",
}


def _normalise_provider(value: str | None) -> ProviderId:
    if value == "codex":
        return "codex"
    return "anthropic"


def _plan_type_from_id_token(id_token: str | None) -> str | None:
    if not id_token:
        return None
    try:
        claims = decode_jwt_payload(id_token)
    except Exception:
        return None
    auth = claims.get("https://api.openai.com/auth") or {}
    if isinstance(auth, dict):
        return auth.get("chatgpt_plan_type") or claims.get("chatgpt_plan_type")
    return claims.get("chatgpt_plan_type")


def token_to_storage(token: TokenData) -> dict[str, Any]:
    return {
        "access_token": token.access_token,
        "refresh_token": token.refresh_token,
        "last_refresh": token.last_refresh_at or now_iso(),
        "email": token.email,
        "type": "claude" if token.provider == "anthropic" else token.provider,
        "expired": token.expires_at,
        "account_uuid": token.account_uuid,
        "id_token": token.id_token,
        "plan_type": token.plan_type,
    }


def storage_to_token(storage: dict[str, Any]) -> TokenData:
    provider = _normalise_provider(storage.get("type"))
    id_token = storage.get("id_token")
    return TokenData(
        access_token=str(storage["access_token"]),
        refresh_token=str(storage["refresh_token"]),
        email=str(storage.get("email") or "unknown"),
        expires_at=str(storage["expired"]),
        account_uuid=str(storage.get("account_uuid") or ""),
        provider=provider,
        id_token=id_token,
        last_refresh_at=storage.get("last_refresh"),
        plan_type=storage.get("plan_type") or _plan_type_from_id_token(id_token),
    )


def save_token(auth_dir: str, token: TokenData) -> Path:
    directory = Path(auth_dir)
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    filename = f"{PREFIX_BY_PROVIDER[token.provider]}-{sanitize_email(token.email)}.json"
    path = directory / filename
    path.write_text(json.dumps(token_to_storage(token), indent=2), encoding="utf-8")
    os.chmod(path, 0o600)
    return path


def load_all_tokens(auth_dir: str, provider: ProviderId | None = None) -> list[TokenData]:
    directory = Path(auth_dir)
    if not directory.exists():
        return []
    prefix = PREFIX_BY_PROVIDER[provider] if provider else None
    tokens: list[TokenData] = []
    for path in sorted(directory.glob("*.json")):
        if prefix and not path.name.startswith(f"{prefix}-"):
            continue
        if not prefix and not (path.name.startswith("claude-") or path.name.startswith("codex-")):
            continue
        try:
            token = storage_to_token(json.loads(path.read_text(encoding="utf-8")))
        except Exception as exc:
            logger.warning("failed to load token file %s: %s", path.name, exc)
            continue
        if provider and token.provider != provider:
            continue
        tokens.append(token)
    return tokens
