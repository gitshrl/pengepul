from __future__ import annotations

import json
from pathlib import Path

import pytest

from pengepul.config import Config
from pengepul.pi_config import (
    build_pi_models_config,
    install_pi_models_config,
    pi_models_path,
)


def test_pi_models_path_uses_home_pi_agent_directory(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))

    assert pi_models_path() == tmp_path / ".pi" / "agent" / "models.json"


def test_build_pi_models_config_targets_pengepul_providers() -> None:
    payload = build_pi_models_config(
        base_url="http://127.0.0.1:8317/",
        api_key_command="!pengepul config api-key",
    )

    providers = payload["providers"]
    anthropic = providers["pengepul-anthropic"]
    codex = providers["pengepul-codex"]

    assert anthropic["baseUrl"] == "http://127.0.0.1:8317"
    assert anthropic["api"] == "anthropic-messages"
    assert anthropic["apiKey"] == "!pengepul config api-key"
    assert anthropic["authHeader"] is True
    assert anthropic["compat"] == {"supportsEagerToolInputStreaming": False}
    assert [model["id"] for model in anthropic["models"]] == ["sonnet", "opus", "haiku"]
    assert [model["name"] for model in anthropic["models"]] == [
        "Claude Sonnet 4.6 via Pengepul",
        "Claude Opus 4.7 via Pengepul",
        "Claude Haiku 4.5 via Pengepul",
    ]

    assert codex["baseUrl"] == "http://127.0.0.1:8317/v1"
    assert codex["api"] == "openai-responses"
    assert codex["apiKey"] == "!pengepul config api-key"
    assert codex["authHeader"] is True
    assert [model["id"] for model in codex["models"]] == ["gpt-5.5", "gpt-5.4"]
    expected_thinking = {
        "off": "none",
        "minimal": "low",
        "xhigh": "xhigh",
    }
    assert [model["thinkingLevelMap"] for model in codex["models"]] == [
        expected_thinking,
        expected_thinking,
    ]


def test_install_pi_models_config_merges_existing_providers(tmp_path: Path) -> None:
    target = tmp_path / "models.json"
    target.write_text(
        json.dumps(
            {
                "providers": {
                    "ollama": {
                        "baseUrl": "http://localhost:11434/v1",
                        "api": "openai-completions",
                        "apiKey": "ollama",
                        "models": [{"id": "qwen2.5-coder:7b"}],
                    }
                }
            }
        ),
        encoding="utf-8",
    )

    path = install_pi_models_config(
        Config(host="", port=8317),
        config_path=None,
        target_path=target,
        base_url=None,
    )

    assert path == target
    saved = json.loads(target.read_text(encoding="utf-8"))
    assert "ollama" in saved["providers"]
    assert saved["providers"]["pengepul-anthropic"]["baseUrl"] == "http://127.0.0.1:8317"
    assert saved["providers"]["pengepul-codex"]["baseUrl"] == "http://127.0.0.1:8317/v1"


def test_install_pi_models_config_treats_empty_file_as_new_config(tmp_path: Path) -> None:
    target = tmp_path / "models.json"
    target.write_text("", encoding="utf-8")

    install_pi_models_config(
        Config(host="", port=8317),
        config_path=None,
        target_path=target,
        base_url=None,
    )

    saved = json.loads(target.read_text(encoding="utf-8"))
    assert sorted(saved["providers"]) == ["pengepul-anthropic", "pengepul-codex"]


def test_install_pi_models_config_uses_custom_config_in_api_key_command(tmp_path: Path) -> None:
    target = tmp_path / "models.json"
    config_path = tmp_path / "custom config.yaml"

    install_pi_models_config(
        Config(host="0.0.0.0", port=9000),
        config_path=str(config_path),
        target_path=target,
        base_url=None,
    )

    saved = json.loads(target.read_text(encoding="utf-8"))
    assert saved["providers"]["pengepul-codex"]["apiKey"] == (
        f"!pengepul --config '{config_path}' config api-key"
    )
    assert saved["providers"]["pengepul-codex"]["baseUrl"] == "http://127.0.0.1:9000/v1"


def test_install_pi_models_config_rejects_non_object_json(tmp_path: Path) -> None:
    target = tmp_path / "models.json"
    target.write_text("[]", encoding="utf-8")

    with pytest.raises(ValueError, match="must contain a JSON object"):
        install_pi_models_config(
            Config(host="", port=8317),
            config_path=None,
            target_path=target,
            base_url=None,
        )
