from __future__ import annotations

import asyncio
from urllib.parse import quote, urlencode

import httpx

from .types import PKCECodes, RefreshTokenExhaustedError, TokenData
from .utils import decode_jwt_payload, expires_in_iso

ANTHROPIC_AUTH_URL = "https://claude.ai/oauth/authorize"
ANTHROPIC_TOKEN_URL = "https://api.anthropic.com/v1/oauth/token"
ANTHROPIC_CLIENT_ID = "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
ANTHROPIC_REDIRECT_URI = "http://localhost:54545/callback"
ANTHROPIC_SCOPE = "org:create_api_key user:profile user:inference"

CODEX_ISSUER = "https://auth.openai.com"
CODEX_AUTH_URL = f"{CODEX_ISSUER}/oauth/authorize"
CODEX_TOKEN_URL = f"{CODEX_ISSUER}/oauth/token"
CODEX_CLIENT_ID = "app_EMoamEEZ73f0CkXaXp7hrann"
CODEX_CALLBACK_PORT = 1455
CODEX_CALLBACK_PATH = "/auth/callback"
CODEX_REDIRECT_URI = f"http://localhost:{CODEX_CALLBACK_PORT}{CODEX_CALLBACK_PATH}"
CODEX_SCOPE = "openid profile email offline_access api.connectors.read api.connectors.invoke"
CODEX_ORIGINATOR = "codex_cli_rs"


def detect_exhausted_reason(body: str) -> str | None:
    lowered = body.lower()
    for marker in (
        "refresh_token_reused",
        "invalid_grant",
        "expired",
        "invalidated",
        "revoked",
    ):
        if marker in lowered:
            return marker
    return None


def generate_anthropic_auth_url(state: str, pkce: PKCECodes) -> str:
    params = urlencode(
        {
            "code": "true",
            "client_id": ANTHROPIC_CLIENT_ID,
            "response_type": "code",
            "redirect_uri": ANTHROPIC_REDIRECT_URI,
            "code_challenge": pkce.code_challenge,
            "code_challenge_method": "S256",
            "state": state,
        }
    )
    scope = "+".join(quote(part, safe=":") for part in ANTHROPIC_SCOPE.split())
    return f"{ANTHROPIC_AUTH_URL}?{params}&scope={scope}"


def generate_codex_auth_url(state: str, pkce: PKCECodes) -> str:
    params = urlencode(
        {
            "response_type": "code",
            "client_id": CODEX_CLIENT_ID,
            "redirect_uri": CODEX_REDIRECT_URI,
            "scope": CODEX_SCOPE,
            "code_challenge": pkce.code_challenge,
            "code_challenge_method": "S256",
            "id_token_add_organizations": "true",
            "codex_cli_simplified_flow": "true",
            "state": state,
            "originator": CODEX_ORIGINATOR,
        }
    )
    return f"{CODEX_AUTH_URL}?{params}"


def _raise_state_mismatch(returned_state: str, expected_state: str) -> None:
    if returned_state != expected_state:
        raise ValueError("OAuth state mismatch")


def _anthropic_token(data: dict) -> TokenData:
    account = data.get("account") or {}
    return TokenData(
        access_token=data["access_token"],
        refresh_token=data["refresh_token"],
        email=account.get("email_address") or "unknown",
        expires_at=expires_in_iso(data.get("expires_in")),
        account_uuid=account.get("uuid") or "",
        provider="anthropic",
    )


def _codex_identity(id_token: str) -> tuple[str, str, str | None]:
    claims = decode_jwt_payload(id_token)
    auth = claims.get("https://api.openai.com/auth") or {}
    email = claims.get("email") or "unknown"
    account_id = ""
    plan_type = None
    if isinstance(auth, dict):
        account_id = auth.get("chatgpt_account_id") or ""
        plan_type = auth.get("chatgpt_plan_type")
    account_id = account_id or claims.get("chatgpt_account_id") or ""
    plan_type = plan_type or claims.get("chatgpt_plan_type")
    return str(email), str(account_id), str(plan_type) if plan_type else None


def _codex_token(data: dict) -> TokenData:
    id_token = data["id_token"]
    email, account_id, plan_type = _codex_identity(id_token)
    return TokenData(
        access_token=data["access_token"],
        refresh_token=data["refresh_token"],
        email=email,
        expires_at=expires_in_iso(data.get("expires_in"), default_seconds=3600),
        account_uuid=account_id,
        provider="codex",
        id_token=id_token,
        plan_type=plan_type,
    )


async def exchange_anthropic_code(
    code: str,
    returned_state: str,
    expected_state: str,
    pkce: PKCECodes,
) -> TokenData:
    _raise_state_mismatch(returned_state, expected_state)
    async with httpx.AsyncClient(timeout=30) as client:
        resp = await client.post(
            ANTHROPIC_TOKEN_URL,
            json={
                "code": code,
                "grant_type": "authorization_code",
                "client_id": ANTHROPIC_CLIENT_ID,
                "redirect_uri": ANTHROPIC_REDIRECT_URI,
                "code_verifier": pkce.code_verifier,
                "state": expected_state,
            },
        )
    if resp.status_code >= 400:
        raise RuntimeError(f"token exchange failed ({resp.status_code}): {resp.text}")
    return _anthropic_token(resp.json())


async def refresh_anthropic_tokens(refresh_token: str) -> TokenData:
    async with httpx.AsyncClient(timeout=30) as client:
        resp = await client.post(
            ANTHROPIC_TOKEN_URL,
            json={
                "client_id": ANTHROPIC_CLIENT_ID,
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
            },
        )
    if resp.status_code >= 400:
        reason = detect_exhausted_reason(resp.text)
        if reason:
            raise RefreshTokenExhaustedError(reason, resp.status_code, resp.text)
        raise RuntimeError(f"token refresh failed ({resp.status_code}): {resp.text}")
    return _anthropic_token(resp.json())


async def exchange_codex_code(
    code: str,
    returned_state: str,
    expected_state: str,
    pkce: PKCECodes,
) -> TokenData:
    _raise_state_mismatch(returned_state, expected_state)
    async with httpx.AsyncClient(timeout=30) as client:
        resp = await client.post(
            CODEX_TOKEN_URL,
            data={
                "grant_type": "authorization_code",
                "code": code,
                "redirect_uri": CODEX_REDIRECT_URI,
                "client_id": CODEX_CLIENT_ID,
                "code_verifier": pkce.code_verifier,
            },
            headers={"Content-Type": "application/x-www-form-urlencoded"},
        )
    if resp.status_code >= 400:
        raise RuntimeError(f"codex token exchange failed ({resp.status_code}): {resp.text}")
    return _codex_token(resp.json())


async def refresh_codex_tokens(refresh_token: str) -> TokenData:
    async with httpx.AsyncClient(timeout=30) as client:
        resp = await client.post(
            CODEX_TOKEN_URL,
            json={
                "client_id": CODEX_CLIENT_ID,
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
            },
        )
    if resp.status_code >= 400:
        reason = detect_exhausted_reason(resp.text)
        if reason:
            raise RefreshTokenExhaustedError(reason, resp.status_code, resp.text)
        raise RuntimeError(f"codex token refresh failed ({resp.status_code}): {resp.text}")
    return _codex_token(resp.json())


async def refresh_with_retry(refresh_token: str, provider: str, max_retries: int = 3) -> TokenData:
    last_error: BaseException | None = None
    for attempt in range(1, max_retries + 1):
        try:
            if provider == "codex":
                return await refresh_codex_tokens(refresh_token)
            return await refresh_anthropic_tokens(refresh_token)
        except RefreshTokenExhaustedError:
            raise
        except Exception as exc:
            last_error = exc
            if attempt == max_retries:
                break
            await asyncio.sleep(attempt)
    assert last_error is not None
    raise last_error
