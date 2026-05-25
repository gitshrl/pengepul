from __future__ import annotations

import re
from collections.abc import Awaitable, Callable
from dataclasses import dataclass

from .accounts import AccountManager, RefreshPolicy
from .oauth import (
    CODEX_CALLBACK_PATH,
    CODEX_CALLBACK_PORT,
    exchange_anthropic_code,
    exchange_codex_code,
    generate_anthropic_auth_url,
    generate_codex_auth_url,
    refresh_with_retry,
)
from .translate import resolve_model
from .types import PKCECodes, ProviderId, TokenData
from .upstream import list_codex_models

ANTHROPIC_MODELS = [
    "claude-opus-4-7",
    "claude-opus-4-6",
    "claude-sonnet-4-6",
    "claude-haiku-4-5-20251001",
    "claude-haiku-4-5",
    "opus",
    "sonnet",
    "haiku",
]


@dataclass(slots=True)
class OAuthInfo:
    callback_port: int
    callback_path: str


@dataclass(slots=True)
class Provider:
    id: ProviderId
    native_format: str
    manager: AccountManager
    oauth: OAuthInfo
    matches_model: Callable[[str], bool]
    build_auth_url: Callable[[str, PKCECodes], str]
    exchange_code: Callable[[str, str, str, PKCECodes], Awaitable[TokenData]]
    list_models: Callable[[], Awaitable[list[dict[str, str]]]]


class ProviderRegistry:
    def __init__(self, providers: list[Provider]) -> None:
        self._providers = providers
        self._by_id = {provider.id: provider for provider in providers}

    def get(self, provider_id: ProviderId) -> Provider:
        return self._by_id[provider_id]

    def all(self) -> list[Provider]:
        return list(self._providers)

    def with_accounts(self) -> list[Provider]:
        return [provider for provider in self._providers if provider.manager.account_count > 0]

    def for_model(self, model: str) -> Provider:
        resolved = resolve_model(model)
        codex = self._by_id["codex"]
        anthropic = self._by_id["anthropic"]
        if codex.matches_model(resolved):
            return codex
        if anthropic.matches_model(resolved):
            return anthropic
        return anthropic


def build_registry(auth_dir: str) -> ProviderRegistry:
    anthropic_manager = AccountManager(
        auth_dir,
        "anthropic",
        refresh=lambda refresh_token: refresh_with_retry(refresh_token, "anthropic"),
    )
    codex_manager = AccountManager(
        auth_dir,
        "codex",
        refresh=lambda refresh_token: refresh_with_retry(refresh_token, "codex"),
        refresh_policy=RefreshPolicy(kind="since-last-refresh", seconds=8 * 24 * 60 * 60),
    )

    anthropic = Provider(
        id="anthropic",
        native_format="anthropic-messages",
        manager=anthropic_manager,
        oauth=OAuthInfo(callback_port=54545, callback_path="/callback"),
        matches_model=lambda model: bool(re.match(r"^claude-", model, re.I)),
        build_auth_url=generate_anthropic_auth_url,
        exchange_code=exchange_anthropic_code,
        list_models=lambda: _static_models("anthropic", ANTHROPIC_MODELS),
    )
    codex = Provider(
        id="codex",
        native_format="openai-responses",
        manager=codex_manager,
        oauth=OAuthInfo(callback_port=CODEX_CALLBACK_PORT, callback_path=CODEX_CALLBACK_PATH),
        matches_model=lambda model: bool(
            re.match(r"^(gpt-5(\.|-)|gpt-5$|o\d|codex-)", model, re.I)
        ),
        build_auth_url=generate_codex_auth_url,
        exchange_code=exchange_codex_code,
        list_models=lambda: list_codex_models(codex_manager),
    )
    return ProviderRegistry([anthropic, codex])


async def _static_models(owner: str, models: list[str]) -> list[dict[str, str]]:
    return [{"id": model, "owned_by": owner} for model in models]
