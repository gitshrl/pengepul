from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Literal

ProviderId = Literal["anthropic", "codex"]
FailureKind = Literal["rate_limit", "auth", "forbidden", "server", "network"]


@dataclass(slots=True)
class PKCECodes:
    code_verifier: str
    code_challenge: str


@dataclass(slots=True)
class TokenData:
    access_token: str
    refresh_token: str
    email: str
    expires_at: str
    account_uuid: str
    provider: ProviderId
    id_token: str | None = None
    last_refresh_at: str | None = None
    plan_type: str | None = None


@dataclass(slots=True)
class UsageData:
    input_tokens: int = 0
    output_tokens: int = 0
    cache_creation_input_tokens: int = 0
    cache_read_input_tokens: int = 0
    reasoning_output_tokens: int = 0


@dataclass(slots=True)
class AvailableAccount:
    token: TokenData
    device_id: str
    account_uuid: str
    provider: ProviderId
    chatgpt_account_id: str | None = None


class RefreshTokenExhaustedError(RuntimeError):
    def __init__(self, reason: str, status_code: int | None = None, body: str | None = None):
        super().__init__(reason)
        self.reason = reason
        self.status_code = status_code
        self.body = body


def extract_usage(payload: dict[str, Any] | None) -> UsageData:
    usage = (payload or {}).get("usage") or (payload or {}).get("response", {}).get("usage")
    if not isinstance(usage, dict):
        return UsageData()
    input_details = usage.get("input_tokens_details") or {}
    output_details = usage.get("output_tokens_details") or {}
    return UsageData(
        input_tokens=int(usage.get("input_tokens") or 0),
        output_tokens=int(usage.get("output_tokens") or 0),
        cache_creation_input_tokens=int(usage.get("cache_creation_input_tokens") or 0),
        cache_read_input_tokens=int(
            usage.get("cache_read_input_tokens") or input_details.get("cached_tokens") or 0
        ),
        reasoning_output_tokens=int(output_details.get("reasoning_tokens") or 0),
    )
