from __future__ import annotations

import json
import os
import shlex
from pathlib import Path
from typing import Any

from .config import Config

ANTHROPIC_PROVIDER_ID = "pengepul-anthropic"
CODEX_PROVIDER_ID = "pengepul-codex"


def pi_models_path() -> Path:
    return Path.home() / ".pi" / "agent" / "models.json"


def build_pi_models_config(base_url: str, api_key_command: str) -> dict[str, Any]:
    root_url = base_url.rstrip("/")
    return {
        "providers": {
            ANTHROPIC_PROVIDER_ID: {
                "baseUrl": root_url,
                "api": "anthropic-messages",
                "apiKey": api_key_command,
                "authHeader": True,
                "compat": {"supportsEagerToolInputStreaming": False},
                "models": [
                    {
                        "id": "sonnet",
                        "name": "Claude Sonnet 4.6 via Pengepul",
                        "reasoning": True,
                        "input": ["text", "image"],
                    },
                    {
                        "id": "opus",
                        "name": "Claude Opus 4.7 via Pengepul",
                        "reasoning": True,
                        "input": ["text", "image"],
                    },
                    {
                        "id": "haiku",
                        "name": "Claude Haiku 4.5 via Pengepul",
                        "reasoning": False,
                        "input": ["text", "image"],
                    },
                ],
            },
            CODEX_PROVIDER_ID: {
                "baseUrl": f"{root_url}/v1",
                "api": "openai-responses",
                "apiKey": api_key_command,
                "authHeader": True,
                "models": [
                    {
                        "id": "gpt-5.5",
                        "name": "GPT-5.5 via Pengepul",
                        "reasoning": True,
                        "thinkingLevelMap": {
                            "off": "none",
                            "minimal": "low",
                            "xhigh": "xhigh",
                        },
                        "input": ["text", "image"],
                    },
                    {
                        "id": "gpt-5.4",
                        "name": "GPT-5.4 via Pengepul",
                        "reasoning": True,
                        "thinkingLevelMap": {
                            "off": "none",
                            "minimal": "low",
                            "xhigh": "xhigh",
                        },
                        "input": ["text", "image"],
                    },
                ],
            },
        }
    }


def install_pi_models_config(
    config: Config,
    *,
    config_path: str | None,
    target_path: Path | None,
    base_url: str | None,
) -> Path:
    path = target_path or pi_models_path()
    existing = _load_existing_config(path)
    generated = build_pi_models_config(
        base_url=base_url or _base_url(config),
        api_key_command=_api_key_command(config_path),
    )
    merged = _merge_models_config(existing, generated)
    _write_json(path, merged)
    return path


def _base_url(config: Config) -> str:
    host = config.host or "127.0.0.1"
    if host in ("0.0.0.0", "::"):
        host = "127.0.0.1"
    elif ":" in host and not host.startswith("["):
        host = f"[{host}]"
    return f"http://{host}:{config.port}"


def _api_key_command(config_path: str | None) -> str:
    command = ["pengepul"]
    if config_path:
        command.extend(["--config", config_path])
    command.extend(["config", "api-key"])
    return "!" + " ".join(shlex.quote(part) for part in command)


def _load_existing_config(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    raw = path.read_text(encoding="utf-8")
    if not raw.strip():
        return {}
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise ValueError(f"{path} contains invalid JSON") from exc
    if not isinstance(parsed, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return parsed


def _merge_models_config(existing: dict[str, Any], generated: dict[str, Any]) -> dict[str, Any]:
    providers = existing.get("providers") or {}
    if not isinstance(providers, dict):
        raise ValueError("models.json providers must be a JSON object")
    merged = dict(existing)
    merged["providers"] = {**providers, **generated["providers"]}
    return merged


def _write_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = path.with_name(f".{path.name}.tmp")
    tmp_path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(tmp_path, path)
