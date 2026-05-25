from __future__ import annotations

import os
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Literal

import yaml

from .utils import generate_api_key, resolve_auth_dir

DebugMode = Literal["off", "errors", "verbose"]


@dataclass(slots=True)
class TimeoutConfig:
    messages_ms: int = 120_000
    stream_messages_ms: int = 600_000
    count_tokens_ms: int = 30_000


@dataclass(slots=True)
class CloakingConfig:
    cli_version: str = "2.1.88"
    entrypoint: str = "cli"
    codex: dict[str, str] = field(default_factory=dict)


@dataclass(slots=True)
class Config:
    host: str = ""
    port: int = 8317
    auth_dir: str = "~/.pengepul"
    api_keys: set[str] = field(default_factory=set)
    body_limit: str = "200mb"
    cloaking: CloakingConfig = field(default_factory=CloakingConfig)
    timeouts: TimeoutConfig = field(default_factory=TimeoutConfig)
    stats_enabled: bool = True
    debug: DebugMode = "off"


DEFAULT_RAW: dict[str, Any] = {
    "host": "",
    "port": 8317,
    "auth-dir": "~/.pengepul",
    "api-keys": [],
    "body-limit": "200mb",
    "cloaking": {
        "cli-version": "2.1.88",
        "entrypoint": "cli",
        "codex": {},
    },
    "timeouts": {
        "messages-ms": 120_000,
        "stream-messages-ms": 600_000,
        "count-tokens-ms": 30_000,
    },
    "stats": {"enabled": True},
    "debug": "off",
}


def default_config_path() -> Path:
    return Path.home() / ".pengepul" / "config.yaml"


def normalize_debug(value: object) -> DebugMode:
    if value is True:
        return "errors"
    if value in ("errors", "verbose", "off"):
        return value  # type: ignore[return-value]
    return "off"


def is_debug_level(debug: DebugMode, level: Literal["errors", "verbose"]) -> bool:
    if debug == "verbose":
        return True
    return debug == level


def _deep_merge(base: dict[str, Any], override: dict[str, Any]) -> dict[str, Any]:
    out = dict(base)
    for key, value in override.items():
        if isinstance(value, dict) and isinstance(out.get(key), dict):
            out[key] = _deep_merge(out[key], value)
        else:
            out[key] = value
    return out


def load_config(config_path: str | None = None) -> Config:
    path = Path(config_path).expanduser() if config_path else default_config_path()
    if path.exists():
        parsed = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
        if not isinstance(parsed, dict):
            raise ValueError(f"{path} must contain a YAML mapping")
        raw = _deep_merge(DEFAULT_RAW, parsed)
    else:
        raw = dict(DEFAULT_RAW)

    keys = list(raw.get("api-keys") or [])
    if not keys:
        keys = [generate_api_key()]
        raw["api-keys"] = keys
        if config_path is None:
            path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            os.chmod(path.parent, 0o700)
        elif str(path.parent) != ".":
            path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(yaml.safe_dump(raw, sort_keys=False), encoding="utf-8")
        os.chmod(path, 0o600)
        print(f"\ngenerated API key and saved it to {path}:\n\n  {keys[0]}\n")

    cloaking = raw.get("cloaking") or {}
    timeouts = raw.get("timeouts") or {}
    stats = raw.get("stats") or {}
    return Config(
        host=str(raw.get("host") or ""),
        port=int(raw.get("port") or 8317),
        auth_dir=resolve_auth_dir(str(raw.get("auth-dir") or "~/.pengepul")),
        api_keys=set(str(k) for k in keys),
        body_limit=str(raw.get("body-limit") or "200mb"),
        cloaking=CloakingConfig(
            cli_version=str(cloaking.get("cli-version") or "2.1.88"),
            entrypoint=str(cloaking.get("entrypoint") or "cli"),
            codex=dict(cloaking.get("codex") or {}),
        ),
        timeouts=TimeoutConfig(
            messages_ms=int(timeouts.get("messages-ms") or 120_000),
            stream_messages_ms=int(timeouts.get("stream-messages-ms") or 600_000),
            count_tokens_ms=int(timeouts.get("count-tokens-ms") or 30_000),
        ),
        stats_enabled=bool(stats.get("enabled", True)),
        debug=normalize_debug(raw.get("debug")),
    )
