from __future__ import annotations

from typing import Any

import httpx


class AdminClientError(RuntimeError):
    pass


def get_health(base_url: str) -> dict[str, Any]:
    return _request("GET", base_url, "/health", api_key=None)


def get_accounts(base_url: str, api_key: str) -> dict[str, Any]:
    return _request("GET", base_url, "/admin/accounts", api_key=api_key)


def reload_accounts(base_url: str, api_key: str) -> dict[str, Any]:
    return _request("POST", base_url, "/admin/reload", api_key=api_key)


def _request(method: str, base_url: str, path: str, api_key: str | None) -> dict[str, Any]:
    headers = {"Authorization": f"Bearer {api_key}"} if api_key else None
    try:
        response = httpx.request(
            method,
            f"{base_url.rstrip('/')}{path}",
            headers=headers,
            timeout=5,
        )
    except httpx.HTTPError as exc:
        raise AdminClientError(str(exc)) from exc

    if response.status_code >= 400:
        raise AdminClientError(f"HTTP {response.status_code}: {response.text[:200]}")

    try:
        payload = response.json()
    except ValueError as exc:
        raise AdminClientError("server returned non-JSON response") from exc

    if not isinstance(payload, dict):
        raise AdminClientError("server returned non-object JSON response")
    return payload
