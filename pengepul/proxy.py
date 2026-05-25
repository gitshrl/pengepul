from __future__ import annotations

from collections.abc import Awaitable, Callable
from typing import Any

from fastapi.responses import JSONResponse, Response

from .accounts import AccountManager
from .types import AvailableAccount, FailureKind
from .upstream import UpstreamResponse

UpstreamCall = Callable[[AvailableAccount], Awaitable[UpstreamResponse]]
SuccessHandler = Callable[[UpstreamResponse, AvailableAccount], Awaitable[Response]]
ErrorAdapter = Callable[[int, str], dict[str, Any]]


def classify_status(status: int) -> FailureKind:
    if status == 429:
        return "rate_limit"
    if status == 401:
        return "auth"
    if status == 403:
        return "forbidden"
    if status >= 500:
        return "server"
    return "network"


def default_error_body(_status: int, body: str) -> dict[str, Any]:
    return {"error": {"message": body or "upstream request failed"}}


async def proxy_with_retry(
    manager: AccountManager,
    upstream: UpstreamCall,
    success: SuccessHandler,
    error_adapter: ErrorAdapter | None = None,
) -> Response:
    attempts = max(1, manager.account_count)
    last_failure: tuple[int, dict[str, Any]] | None = None
    adapter = error_adapter or default_error_body

    for _ in range(attempts):
        result = manager.get_next_account()
        if not result.account:
            detail: dict[str, Any] = {
                "error": {
                    "message": (
                        f"no available {manager.provider} account; run login for {manager.provider}"
                    ),
                    "type": "no_account_for_provider",
                    "provider": manager.provider,
                }
            }
            if result.failure_kind:
                detail["error"]["failure_kind"] = result.failure_kind
            if result.retry_after_seconds is not None:
                detail["error"]["retry_after_seconds"] = int(result.retry_after_seconds)
            return JSONResponse(detail, status_code=503)

        account = result.account
        manager.record_attempt(account.token.email)
        if not await manager.refresh_if_due(account.token.email):
            continue
        refreshed = manager.get_next_account()
        if refreshed.account:
            account = refreshed.account

        try:
            upstream_response = await upstream(account)
        except Exception as exc:
            manager.record_failure(account.token.email, "network", str(exc))
            last_failure = (502, {"error": {"message": str(exc), "type": "network_error"}})
            continue

        if 200 <= upstream_response.status_code < 300:
            return await success(upstream_response, account)

        text = await upstream_response.text()
        await upstream_response.aclose()
        kind = classify_status(upstream_response.status_code)
        manager.record_failure(account.token.email, kind, text[:500])
        last_failure = (upstream_response.status_code, adapter(upstream_response.status_code, text))
        if kind in ("auth", "forbidden"):
            continue
        if kind in ("rate_limit", "server", "network"):
            continue

    if last_failure:
        return JSONResponse(last_failure[1], status_code=last_failure[0])
    return JSONResponse(
        {
            "error": {
                "message": f"no available {manager.provider} account",
                "type": "no_account_for_provider",
                "provider": manager.provider,
            }
        },
        status_code=503,
    )


def openai_error_body(_status: int, body: str) -> dict[str, Any]:
    import json

    try:
        parsed = json.loads(body)
    except Exception:
        return {"error": {"message": "upstream request failed", "type": "upstream_error"}}
    parsed_error = parsed.get("error")
    message = (
        (parsed_error.get("message") if isinstance(parsed_error, dict) else None)
        or parsed.get("detail")
        or "upstream request failed"
    )
    error_type = (
        parsed_error.get("type") if isinstance(parsed_error, dict) else None
    ) or "upstream_error"
    return {"error": {"message": message, "type": error_type}}
