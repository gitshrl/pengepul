from __future__ import annotations

import stat
from pathlib import Path

from pytest import MonkeyPatch

from pengepul.config import load_config


def test_default_config_is_generated_under_home_pengepul(
    tmp_path: Path, monkeypatch: MonkeyPatch
) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.chdir(workspace)

    config = load_config()

    config_path = tmp_path / ".pengepul" / "config.yaml"
    assert config_path.exists()
    assert not (workspace / "config.yaml").exists()
    assert config.auth_dir == str(tmp_path / ".pengepul")
    assert stat.S_IMODE(config_path.parent.stat().st_mode) == 0o700
    assert stat.S_IMODE(config_path.stat().st_mode) == 0o600


def test_default_config_migrates_legacy_workspace_config(
    tmp_path: Path, monkeypatch: MonkeyPatch
) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.chdir(workspace)
    legacy_path = workspace / "config.yaml"
    legacy_path.write_text(
        "\n".join(
            [
                'host: "127.0.0.1"',
                "port: 9000",
                "auth-dir: ~/.pengepul",
                "api-keys:",
                "  - sk-legacy",
                "",
            ]
        ),
        encoding="utf-8",
    )

    config = load_config()

    config_path = tmp_path / ".pengepul" / "config.yaml"
    assert config_path.exists()
    assert legacy_path.exists()
    assert config.api_keys == {"sk-legacy"}
    assert config.port == 9000
    assert stat.S_IMODE(config_path.parent.stat().st_mode) == 0o700
    assert stat.S_IMODE(config_path.stat().st_mode) == 0o600


def test_explicit_config_path_is_respected(tmp_path: Path, monkeypatch: MonkeyPatch) -> None:
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    config_path = tmp_path / "custom.yaml"

    config = load_config(str(config_path))

    assert config_path.exists()
    assert not (tmp_path / "home" / ".pengepul" / "config.yaml").exists()
    assert config.auth_dir == str(tmp_path / "home" / ".pengepul")
