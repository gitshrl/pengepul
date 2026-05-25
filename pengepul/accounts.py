from __future__ import annotations

import asyncio
import logging
import random
import time
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from typing import Literal

from .tokens import load_all_tokens, save_token
from .types import AvailableAccount, FailureKind, RefreshTokenExhaustedError, TokenData, UsageData
from .utils import get_device_id, iso_to_timestamp, now_iso

RefreshFn = Callable[[str], Awaitable[TokenData]]
RefreshPolicyKind = Literal["expires-lead", "since-last-refresh"]

REAUTH_COOLDOWN_SECONDS = 24 * 60 * 60
DEFAULT_REFRESH_LEAD_SECONDS = 4 * 60 * 60
STICKY_MIN_SECONDS = 20 * 60
STICKY_MAX_SECONDS = 60 * 60

FAILURE_BACKOFF: dict[FailureKind, tuple[int, int]] = {
    "rate_limit": (60, 15 * 60),
    "auth": (10 * 60, 60 * 60),
    "forbidden": (10 * 60, 60 * 60),
    "server": (5, 5 * 60),
    "network": (5, 5 * 60),
}

FAILURE_PRIORITY: dict[FailureKind, int] = {
    "rate_limit": 0,
    "server": 1,
    "network": 2,
    "forbidden": 3,
    "auth": 4,
}

logger = logging.getLogger(__name__)


@dataclass(slots=True)
class RefreshPolicy:
    kind: RefreshPolicyKind = "expires-lead"
    seconds: int = DEFAULT_REFRESH_LEAD_SECONDS


@dataclass(slots=True)
class AccountState:
    token: TokenData
    cooldown_until: float = 0.0
    failure_count: int = 0
    last_failure_kind: FailureKind | None = None
    last_error: str | None = None
    last_failure_at: str | None = None
    last_success_at: str | None = None
    last_refresh_at: str | None = None
    total_requests: int = 0
    total_successes: int = 0
    total_failures: int = 0
    total_input_tokens: int = 0
    total_output_tokens: int = 0
    total_cache_creation_input_tokens: int = 0
    total_cache_read_input_tokens: int = 0
    total_reasoning_output_tokens: int = 0
    refresh_task: asyncio.Task[bool] | None = None


@dataclass(slots=True)
class AccountResult:
    account: AvailableAccount | None
    failure_kind: FailureKind | None = None
    retry_after_seconds: float | None = None


class AccountManager:
    def __init__(
        self,
        auth_dir: str,
        provider: Literal["anthropic", "codex"],
        refresh: RefreshFn,
        refresh_policy: RefreshPolicy | None = None,
    ) -> None:
        self.auth_dir = auth_dir
        self.provider = provider
        self.refresh = refresh
        self.refresh_policy = refresh_policy or RefreshPolicy()
        self._accounts: dict[str, AccountState] = {}
        self._order: list[str] = []
        self._last_used_index = -1
        self._sticky_until = 0.0

    @property
    def account_count(self) -> int:
        return len(self._accounts)

    def load(self) -> None:
        for token in load_all_tokens(self.auth_dir, self.provider):
            self._upsert_loaded_token(token)
        logger.info("%s loaded %d account(s)", self.provider, len(self._accounts))

    def reload(self) -> dict[str, list[str]]:
        stats: dict[str, list[str]] = {"added": [], "updated": [], "unchanged": []}
        for token in load_all_tokens(self.auth_dir, self.provider):
            existing = self._accounts.get(token.email)
            if not existing:
                self._upsert_loaded_token(token)
                stats["added"].append(token.email)
                continue
            changed = (
                existing.token.access_token != token.access_token
                or existing.token.refresh_token != token.refresh_token
            )
            if not changed:
                stats["unchanged"].append(token.email)
                continue
            existing.token = token
            existing.cooldown_until = 0.0
            existing.failure_count = 0
            existing.last_failure_kind = None
            existing.last_error = None
            existing.last_failure_at = None
            stats["updated"].append(token.email)
        return stats

    def add_account(self, token: TokenData) -> None:
        if token.provider != self.provider:
            raise ValueError(f"token provider {token.provider} does not match {self.provider}")
        now = now_iso()
        existing = self._accounts.get(token.email)
        if existing:
            existing.token = token
            existing.cooldown_until = 0.0
            existing.failure_count = 0
            existing.last_failure_kind = None
            existing.last_error = None
            existing.last_failure_at = None
            existing.last_success_at = now
            existing.last_refresh_at = now
        else:
            state = self._new_state(token)
            state.last_success_at = now
            state.last_refresh_at = now
            self._accounts[token.email] = state
            self._order.append(token.email)
        save_token(self.auth_dir, token)

    def get_next_account(self) -> AccountResult:
        if not self._order:
            return AccountResult(account=None)

        now = time.monotonic()

        if self._last_used_index >= 0 and now < self._sticky_until:
            email = self._order[self._last_used_index]
            state = self._accounts[email]
            if state.cooldown_until <= now:
                return AccountResult(account=self._available(email, state.token))

        start = self._last_used_index + 1 if self._last_used_index >= 0 else 0
        for offset in range(len(self._order)):
            idx = (start + offset) % len(self._order)
            email = self._order[idx]
            state = self._accounts[email]
            if state.cooldown_until <= now:
                self._last_used_index = idx
                self._sticky_until = now + random.uniform(STICKY_MIN_SECONDS, STICKY_MAX_SECONDS)
                return AccountResult(account=self._available(email, state.token))

        best = min(
            (self._accounts[email] for email in self._order),
            key=lambda state: (
                FAILURE_PRIORITY[state.last_failure_kind or "network"],
                max(0.0, state.cooldown_until - now),
            ),
        )
        kind = best.last_failure_kind or "network"
        retry_after = None if kind in ("auth", "forbidden") else max(0.0, best.cooldown_until - now)
        return AccountResult(account=None, failure_kind=kind, retry_after_seconds=retry_after)

    async def refresh_if_due(self, email: str) -> bool:
        state = self._accounts.get(email)
        if not state or not self._should_refresh(state):
            return True
        return await self.refresh_account(email)

    async def refresh_account(self, email: str) -> bool:
        state = self._accounts.get(email)
        if not state:
            return False
        if state.refresh_task is None:
            state.refresh_task = asyncio.create_task(self._perform_refresh(state))
        return await state.refresh_task

    def record_attempt(self, email: str) -> None:
        state = self._accounts.get(email)
        if state:
            state.total_requests += 1

    def record_success(self, email: str, usage: UsageData | None = None) -> None:
        state = self._accounts.get(email)
        if not state:
            return
        state.cooldown_until = 0.0
        state.failure_count = 0
        state.last_failure_kind = None
        state.last_error = None
        state.last_failure_at = None
        state.last_success_at = now_iso()
        state.total_successes += 1
        if usage:
            state.total_input_tokens += usage.input_tokens
            state.total_output_tokens += usage.output_tokens
            state.total_cache_creation_input_tokens += usage.cache_creation_input_tokens
            state.total_cache_read_input_tokens += usage.cache_read_input_tokens
            state.total_reasoning_output_tokens += usage.reasoning_output_tokens

    def record_failure(self, email: str, kind: FailureKind, detail: str | None = None) -> None:
        state = self._accounts.get(email)
        if not state:
            return
        state.failure_count += 1
        state.total_failures += 1
        state.last_failure_kind = kind
        state.last_failure_at = now_iso()
        state.last_error = f"{kind}: {detail}" if detail else kind
        base, maximum = FAILURE_BACKOFF[kind]
        cooldown = min(base * (2 ** max(0, state.failure_count - 1)), maximum)
        now = time.monotonic()
        state.cooldown_until = now + cooldown

    def get_snapshots(self) -> list[dict[str, object]]:
        now_monotonic = time.monotonic()
        now_wall = time.time()
        snapshots: list[dict[str, object]] = []
        for state in self._accounts.values():
            cooldown_remaining = max(0.0, state.cooldown_until - now_monotonic)
            snapshots.append(
                {
                    "email": state.token.email,
                    "available": cooldown_remaining == 0.0,
                    "cooldownUntil": now_wall + cooldown_remaining if cooldown_remaining else 0.0,
                    "failureCount": state.failure_count,
                    "lastError": state.last_error,
                    "lastFailureAt": state.last_failure_at,
                    "lastSuccessAt": state.last_success_at,
                    "lastRefreshAt": state.last_refresh_at,
                    "totalRequests": state.total_requests,
                    "totalSuccesses": state.total_successes,
                    "totalFailures": state.total_failures,
                    "totalInputTokens": state.total_input_tokens,
                    "totalOutputTokens": state.total_output_tokens,
                    "totalCacheCreationInputTokens": state.total_cache_creation_input_tokens,
                    "totalCacheReadInputTokens": state.total_cache_read_input_tokens,
                    "totalReasoningOutputTokens": state.total_reasoning_output_tokens,
                    "expiresAt": state.token.expires_at,
                    "refreshing": state.refresh_task is not None,
                    "planType": state.token.plan_type,
                }
            )
        return snapshots

    async def _perform_refresh(self, state: AccountState) -> bool:
        try:
            refreshed = await self.refresh(state.token.refresh_token)
            refresh_at = now_iso()
            new_token = TokenData(
                access_token=refreshed.access_token,
                refresh_token=refreshed.refresh_token,
                email=refreshed.email or state.token.email,
                expires_at=refreshed.expires_at,
                account_uuid=refreshed.account_uuid or state.token.account_uuid,
                provider=self.provider,
                id_token=refreshed.id_token or state.token.id_token,
                last_refresh_at=refresh_at,
                plan_type=refreshed.plan_type or state.token.plan_type,
            )
            save_token(self.auth_dir, new_token)
            state.token = new_token
            state.cooldown_until = 0.0
            state.failure_count = 0
            state.last_failure_kind = None
            state.last_error = None
            state.last_failure_at = None
            state.last_success_at = refresh_at
            state.last_refresh_at = refresh_at
            return True
        except RefreshTokenExhaustedError as exc:
            state.failure_count += 1
            state.total_failures += 1
            state.last_failure_kind = "auth"
            state.last_failure_at = now_iso()
            state.last_error = f"refresh token {exc.reason}; re-run login for {self.provider}"
            now = time.monotonic()
            state.cooldown_until = now + REAUTH_COOLDOWN_SECONDS
            return False
        except Exception as exc:
            self.record_failure(state.token.email, "auth", str(exc))
            return False
        finally:
            state.refresh_task = None

    def _should_refresh(self, state: AccountState) -> bool:
        if self.refresh_policy.kind == "since-last-refresh":
            if not state.last_refresh_at:
                return False
            elapsed = time.time() - iso_to_timestamp(state.last_refresh_at)
            return elapsed >= self.refresh_policy.seconds
        return iso_to_timestamp(state.token.expires_at) - time.time() <= self.refresh_policy.seconds

    def _upsert_loaded_token(self, token: TokenData) -> None:
        if token.email not in self._accounts:
            self._accounts[token.email] = self._new_state(token)
            self._order.append(token.email)
        else:
            self._accounts[token.email].token = token

    def _new_state(self, token: TokenData) -> AccountState:
        return AccountState(token=token, last_refresh_at=token.last_refresh_at)

    def _available(self, email: str, token: TokenData) -> AvailableAccount:
        return AvailableAccount(
            token=token,
            device_id=get_device_id(self.auth_dir, email),
            account_uuid=token.account_uuid,
            provider=self.provider,
            chatgpt_account_id=token.account_uuid if self.provider == "codex" else None,
        )
