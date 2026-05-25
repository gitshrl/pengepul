from __future__ import annotations

import hashlib
import json
import logging
import platform
import random
import time
import uuid
from collections.abc import Mapping
from copy import deepcopy
from dataclasses import dataclass
from typing import Any

import httpx

from .accounts import AccountManager
from .config import Config
from .types import AvailableAccount
from .utils import extract_api_key, hash_api_key, short_error

ANTHROPIC_BASE_URL = "https://api.anthropic.com"
ANTHROPIC_OAUTH_BETA = "oauth-2025-04-20"
FINGERPRINT_SALT = "59cf53e54c78"

CODEX_BASE_URL = "https://chatgpt.com/backend-api"
CODEX_RESPONSES_PATH = "/codex/responses"
CODEX_MODELS_PATH = "/codex/models"
CODEX_DEFAULT_ORIGINATOR = "codex_cli_rs"
CODEX_DEFAULT_CLI_VERSION = "0.125.0"
CODEX_MODEL_CACHE_TTL = 5 * 60
CODEX_FALLBACK_MODELS = ["gpt-5.5", "gpt-5.4", "gpt-5.4-mini", "gpt-5.3-codex", "gpt-5.2"]

_sessions: dict[str, tuple[str, float, float]] = {}
_codex_models_cache: tuple[float, str | None, list[dict[str, Any]]] | None = None
logger = logging.getLogger(__name__)


@dataclass(slots=True)
class UpstreamResponse:
    response: httpx.Response
    client: httpx.AsyncClient

    @property
    def status_code(self) -> int:
        return self.response.status_code

    @property
    def headers(self) -> httpx.Headers:
        return self.response.headers

    async def text(self) -> str:
        await self.response.aread()
        return self.response.text

    async def json(self) -> Any:
        await self.response.aread()
        return self.response.json()

    async def aiter_bytes(self):
        async for chunk in self.response.aiter_bytes():
            yield chunk

    async def aclose(self) -> None:
        await self.response.aclose()
        await self.client.aclose()


def _session_id(api_key_hash: str) -> str:
    now = time.time()
    current = _sessions.get(api_key_hash)
    if current:
        sid, last_used, ttl = current
        if now - last_used < ttl:
            _sessions[api_key_hash] = (sid, now, ttl)
            return sid
    for key, (_, last_used, ttl) in list(_sessions.items()):
        if now - last_used >= ttl:
            del _sessions[key]
    sid = str(uuid.uuid4())
    _sessions[api_key_hash] = (sid, now, random.uniform(30 * 60, 300 * 60))
    return sid


def _build_beta_header(model: str, structured: bool) -> str:
    is_haiku = "haiku" in model
    common = [
        "oauth-2025-04-20",
        "interleaved-thinking-2025-05-14",
        "redact-thinking-2026-02-12",
        "context-management-2025-06-27",
        "prompt-caching-scope-2026-01-05",
    ]
    if structured:
        extra = ["structured-outputs-2025-12-15"]
    elif is_haiku:
        extra = ["claude-code-20250219"]
    else:
        extra = ["advanced-tool-use-2025-11-20", "effort-2025-11-24"]
    if not is_haiku and not structured:
        common.insert(0, "claude-code-20250219")
    return ",".join(common + extra)


def _stainless_os() -> str:
    system = platform.system().lower()
    if system == "darwin":
        return "MacOS"
    if system == "windows":
        return "Windows"
    if system == "freebsd":
        return "FreeBSD"
    return "Linux"


def _stainless_arch() -> str:
    machine = platform.machine().lower()
    if machine in ("arm64", "aarch64"):
        return "arm64"
    if machine in ("x86_64", "amd64"):
        return "x64"
    return "x86"


def _passthrough_anthropic_headers(headers: Mapping[str, str]) -> dict[str, str]:
    user_agent = headers.get("user-agent", "")
    if not user_agent.lower().startswith("claude-cli"):
        return {}
    out = {key: value for key, value in headers.items() if key.lower().startswith("anthropic")}
    session = headers.get("x-claude-code-session-id")
    if session:
        out["X-Claude-Code-Session-Id"] = session
    return out


def _anthropic_headers(
    *,
    token: str,
    stream: bool,
    timeout_ms: int,
    model: str,
    config: Config,
    request_headers: Mapping[str, str],
    structured: bool = False,
) -> dict[str, str]:
    cli_version = config.cloaking.cli_version
    entrypoint = config.cloaking.entrypoint
    api_hash = hash_api_key(extract_api_key(request_headers))
    headers: dict[str, str] = {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {token}",
        "User-Agent": f"claude-cli/{cli_version} (external, {entrypoint})",
        "X-Claude-Code-Session-Id": _session_id(api_hash),
        "X-Stainless-Lang": "js",
        "X-Stainless-Package-Version": "0.74.0",
        "X-Stainless-Runtime": "node",
        "X-Stainless-Runtime-Version": "v22.13.0",
        "X-Stainless-Arch": _stainless_arch(),
        "X-Stainless-Os": _stainless_os(),
        "X-Stainless-Timeout": str(max(1, int((timeout_ms + 999) / 1000))),
        "X-Stainless-Retry-Count": "0",
        "Accept": "text/event-stream" if stream else "application/json",
        "anthropic-dangerous-direct-browser-access": "true",
        "anthropic-version": "2023-06-01",
        "x-app": "cli",
        "x-client-request-id": str(uuid.uuid4()),
    }
    headers.update(_passthrough_anthropic_headers(request_headers))
    beta = headers.get("anthropic-beta")
    if beta:
        parts = [part.strip() for part in beta.split(",") if part.strip()]
        if ANTHROPIC_OAUTH_BETA not in parts:
            parts.insert(0, ANTHROPIC_OAUTH_BETA)
        headers["anthropic-beta"] = ",".join(dict.fromkeys(parts))
    else:
        headers["anthropic-beta"] = _build_beta_header(model, structured)
    return headers


def _billing_header(messages: list[dict[str, Any]], version: str, entrypoint: str) -> str:
    text = ""
    for message in messages:
        if message.get("role") == "user":
            content = message.get("content")
            if isinstance(content, str):
                text = content
            elif isinstance(content, list):
                for block in content:
                    if isinstance(block, dict) and block.get("type") == "text":
                        text = block.get("text") or ""
                        break
            break
    chars = "".join(text[i] if i < len(text) else "0" for i in (4, 7, 20))
    fingerprint = hashlib.sha256(f"{FINGERPRINT_SALT}{chars}{version}".encode()).hexdigest()[:3]
    return (
        f"x-anthropic-billing-header: cc_version={version}.{fingerprint}; "
        f"cc_entrypoint={entrypoint};"
    )


def apply_cloaking(
    body: dict[str, Any],
    *,
    request_headers: Mapping[str, str],
    account: AvailableAccount,
    config: Config,
) -> dict[str, Any]:
    next_body = deepcopy(body)
    existing = next_body.get("system") or []
    remaining = (
        list(existing) if isinstance(existing, list) else [{"type": "text", "text": str(existing)}]
    )

    billing = None
    prefix = None
    kept: list[dict[str, Any]] = []
    for block in remaining:
        text = block.get("text") if isinstance(block, dict) else ""
        if isinstance(text, str) and "x-anthropic-billing-header" in text and billing is None:
            billing = block
        elif isinstance(text, str) and "You are Claude Code" in text and prefix is None:
            prefix = block
        else:
            kept.append(block)
    if billing is None:
        billing = {
            "type": "text",
            "text": _billing_header(
                next_body.get("messages") or [],
                config.cloaking.cli_version,
                config.cloaking.entrypoint,
            ),
        }
    if prefix is None:
        prefix = {
            "type": "text",
            "text": "You are Claude Code, Anthropic's official CLI for Claude.",
            "cache_control": {"type": "ephemeral"},
        }
    next_body["system"] = [billing, prefix, *kept]

    session = request_headers.get("x-claude-code-session-id")
    if not session:
        session = _session_id(hash_api_key(extract_api_key(request_headers)))
    next_body.setdefault("metadata", {})
    next_body["metadata"]["user_id"] = json.dumps(
        {
            "device_id": account.device_id,
            "account_uuid": account.account_uuid,
            "session_id": session,
        }
    )
    return next_body


async def _send_streaming_json(
    url: str,
    *,
    headers: dict[str, str],
    body: dict[str, Any],
    timeout_ms: int,
) -> UpstreamResponse:
    timeout = httpx.Timeout(timeout_ms / 1000, connect=30)
    client = httpx.AsyncClient(timeout=timeout)
    request = client.build_request("POST", url, headers=headers, json=body)
    try:
        response = await client.send(request, stream=True)
    except Exception:
        await client.aclose()
        raise
    return UpstreamResponse(response=response, client=client)


async def call_anthropic_messages(
    *,
    body: dict[str, Any],
    request_headers: Mapping[str, str],
    account: AvailableAccount,
    config: Config,
    structured: bool = False,
) -> UpstreamResponse:
    stream = bool(body.get("stream"))
    timeout_ms = config.timeouts.stream_messages_ms if stream else config.timeouts.messages_ms
    model = body.get("model") or "claude-sonnet-4-6"
    return await _send_streaming_json(
        f"{ANTHROPIC_BASE_URL}/v1/messages?beta=true",
        headers=_anthropic_headers(
            token=account.token.access_token,
            stream=stream,
            timeout_ms=timeout_ms,
            model=model,
            config=config,
            request_headers=request_headers,
            structured=structured,
        ),
        body=body,
        timeout_ms=timeout_ms,
    )


async def call_anthropic_count_tokens(
    *,
    body: dict[str, Any],
    request_headers: Mapping[str, str],
    account: AvailableAccount,
    config: Config,
) -> UpstreamResponse:
    model = body.get("model") or "claude-sonnet-4-6"
    timeout_ms = config.timeouts.count_tokens_ms
    return await _send_streaming_json(
        f"{ANTHROPIC_BASE_URL}/v1/messages/count_tokens?beta=true",
        headers=_anthropic_headers(
            token=account.token.access_token,
            stream=False,
            timeout_ms=timeout_ms,
            model=model,
            config=config,
            request_headers=request_headers,
        ),
        body=body,
        timeout_ms=timeout_ms,
    )


def normalize_codex_responses_body(body: dict[str, Any]) -> dict[str, Any]:
    next_body = dict(body)
    next_body.setdefault("stream", True)
    next_body.setdefault("store", False)
    next_body.setdefault("instructions", "")
    return next_body


def _codex_user_agent(config: Config) -> str:
    codex = config.cloaking.codex
    if codex.get("user-agent"):
        return codex["user-agent"]
    originator = codex.get("originator") or CODEX_DEFAULT_ORIGINATOR
    version = codex.get("cli-version") or CODEX_DEFAULT_CLI_VERSION
    system = platform.system().lower()
    os_name = "macos" if system == "darwin" else "windows" if system == "windows" else "linux"
    machine = platform.machine().lower()
    arch = "arm64" if machine in ("arm64", "aarch64") else "x86_64"
    return f"{originator}/{version} ({os_name}; {arch})"


def _codex_headers(account: AvailableAccount, stream: bool, config: Config) -> dict[str, str]:
    codex = config.cloaking.codex
    version = codex.get("cli-version") or CODEX_DEFAULT_CLI_VERSION
    headers = {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {account.token.access_token}",
        "Accept": "text/event-stream" if stream else "application/json",
        "User-Agent": _codex_user_agent(config),
        "originator": codex.get("originator") or CODEX_DEFAULT_ORIGINATOR,
        "version": version,
    }
    if account.chatgpt_account_id:
        headers["ChatGPT-Account-ID"] = account.chatgpt_account_id
    if codex.get("openai-beta"):
        headers["OpenAI-Beta"] = codex["openai-beta"]
    return headers


async def call_codex_responses(
    *,
    body: dict[str, Any],
    account: AvailableAccount,
    config: Config,
) -> UpstreamResponse:
    stream = bool(body.get("stream"))
    timeout_ms = config.timeouts.stream_messages_ms if stream else config.timeouts.messages_ms
    try:
        return await _send_streaming_json(
            f"{CODEX_BASE_URL}{CODEX_RESPONSES_PATH}",
            headers=_codex_headers(account, stream, config),
            body=body,
            timeout_ms=timeout_ms,
        )
    except Exception as exc:
        raise RuntimeError(f"codex upstream fetch failed: {short_error(exc)}") from exc


async def list_codex_models(manager: AccountManager) -> list[dict[str, str]]:
    global _codex_models_cache
    now = time.time()
    if _codex_models_cache and now - _codex_models_cache[0] < CODEX_MODEL_CACHE_TTL:
        return [{"id": model["slug"], "owned_by": "openai"} for model in _codex_models_cache[2]]

    result = manager.get_next_account()
    if not result.account:
        return [{"id": model, "owned_by": "openai"} for model in CODEX_FALLBACK_MODELS]

    headers = {
        "Authorization": f"Bearer {result.account.token.access_token}",
        "Accept": "application/json",
        "User-Agent": "pengepul/0.1.0",
    }
    if result.account.chatgpt_account_id:
        headers["ChatGPT-Account-ID"] = result.account.chatgpt_account_id
    if _codex_models_cache and _codex_models_cache[1]:
        headers["If-None-Match"] = _codex_models_cache[1]

    async with httpx.AsyncClient(timeout=10) as client:
        try:
            resp = await client.get(
                f"{CODEX_BASE_URL}{CODEX_MODELS_PATH}",
                params={"client_version": "pengepul/0.1.0"},
                headers=headers,
            )
        except Exception as exc:
            logger.warning("codex model list fetch failed: %s", short_error(exc))
            if _codex_models_cache:
                return [
                    {"id": model["slug"], "owned_by": "openai"} for model in _codex_models_cache[2]
                ]
            return [{"id": model, "owned_by": "openai"} for model in CODEX_FALLBACK_MODELS]
    if resp.status_code == 304 and _codex_models_cache:
        return [{"id": model["slug"], "owned_by": "openai"} for model in _codex_models_cache[2]]
    if resp.status_code >= 400:
        logger.warning("codex model list returned %d: %s", resp.status_code, resp.text[:200])
        if _codex_models_cache:
            return [{"id": model["slug"], "owned_by": "openai"} for model in _codex_models_cache[2]]
        return [{"id": model, "owned_by": "openai"} for model in CODEX_FALLBACK_MODELS]
    data = resp.json()
    models = data.get("models") or []
    if not isinstance(models, list):
        return [{"id": model, "owned_by": "openai"} for model in CODEX_FALLBACK_MODELS]
    _codex_models_cache = (now, resp.headers.get("etag"), models)
    return [{"id": model["slug"], "owned_by": "openai"} for model in models if model.get("slug")]
