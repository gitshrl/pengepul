from __future__ import annotations

from pathlib import Path
from typing import Any

from pytest import MonkeyPatch

from pengepul import cli


def _write_config(home: Path, host: str = "127.0.0.1", port: int = 8317) -> None:
    config_dir = home / ".pengepul"
    config_dir.mkdir(parents=True)
    (config_dir / "config.yaml").write_text(
        "\n".join(
            [
                f'host: "{host}"',
                f"port: {port}",
                "auth-dir: ~/.pengepul",
                "api-keys:",
                "  - sk-test",
                "",
            ]
        ),
        encoding="utf-8",
    )


def test_default_command_starts_server(tmp_path: Path, monkeypatch: MonkeyPatch, capsys) -> None:
    _write_config(tmp_path, host="0.0.0.0", port=8318)
    called: dict[str, object] = {}

    def fake_run_server(config, registry) -> None:
        called["host"] = config.host
        called["port"] = config.port
        called["accounts"] = len(registry.all())

    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setattr(cli, "_run_server", fake_run_server)

    assert cli.run([]) == 0

    assert called == {"host": "0.0.0.0", "port": 8318, "accounts": 2}
    assert capsys.readouterr().err == ""


def test_top_level_help_uses_subcommands() -> None:
    help_text = cli._build_parser().format_help()

    assert "--login" not in help_text
    assert "--manual" not in help_text
    assert "--host HOST" not in help_text
    assert "--port PORT" not in help_text
    assert "login" in help_text
    assert "serve" in help_text


def test_serve_subcommand_starts_server_with_custom_host_port(
    tmp_path: Path, monkeypatch: MonkeyPatch
) -> None:
    _write_config(tmp_path)
    called: dict[str, object] = {}

    def fake_run_server(config, _registry) -> None:
        called["host"] = config.host
        called["port"] = config.port

    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setattr(cli, "_run_server", fake_run_server)

    assert cli.run(["serve", "--host", "0.0.0.0", "--port", "9000"]) == 0

    assert called == {"host": "0.0.0.0", "port": 9000}


def test_config_commands_print_path_and_api_key(
    tmp_path: Path, monkeypatch: MonkeyPatch, capsys
) -> None:
    _write_config(tmp_path)
    monkeypatch.setenv("HOME", str(tmp_path))

    assert cli.run(["config", "path"]) == 0
    assert capsys.readouterr().out.strip() == str(tmp_path / ".pengepul" / "config.yaml")

    assert cli.run(["config", "api-key"]) == 0
    assert capsys.readouterr().out.strip() == "sk-test"


def test_config_path_does_not_generate_config(
    tmp_path: Path, monkeypatch: MonkeyPatch, capsys
) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))

    assert cli.run(["config", "path"]) == 0

    assert capsys.readouterr().out.strip() == str(tmp_path / ".pengepul" / "config.yaml")
    assert not (tmp_path / ".pengepul" / "config.yaml").exists()


def test_status_reports_health_and_account_counts(
    tmp_path: Path, monkeypatch: MonkeyPatch, capsys
) -> None:
    _write_config(tmp_path, host="0.0.0.0", port=8318)
    calls: dict[str, object] = {}

    def fake_get_health(base_url: str) -> dict[str, Any]:
        calls["health_url"] = base_url
        return {"status": "ok"}

    def fake_get_accounts(base_url: str, api_key: str) -> dict[str, Any]:
        calls["accounts_url"] = base_url
        calls["api_key"] = api_key
        return {
            "providers": {
                "anthropic": {"account_count": 1, "accounts": []},
                "codex": {"account_count": 2, "accounts": []},
            }
        }

    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setattr(cli, "get_health", fake_get_health)
    monkeypatch.setattr(cli, "get_accounts", fake_get_accounts)

    assert cli.run(["status"]) == 0

    output = capsys.readouterr().out
    assert "config: " in output
    assert "url: http://127.0.0.1:8318" in output
    assert "server: ok" in output
    assert "anthropic: 1 account" in output
    assert "codex: 2 accounts" in output
    assert calls == {
        "health_url": "http://127.0.0.1:8318",
        "accounts_url": "http://127.0.0.1:8318",
        "api_key": "sk-test",
    }


def test_accounts_reload_then_prints_runtime_accounts(
    tmp_path: Path, monkeypatch: MonkeyPatch, capsys
) -> None:
    _write_config(tmp_path)
    calls: list[str] = []

    def fake_reload_accounts(base_url: str, api_key: str) -> dict[str, Any]:
        calls.append(f"reload:{base_url}:{api_key}")
        return {"reloaded": {"anthropic": {"added": [], "updated": [], "unchanged": []}}}

    def fake_get_accounts(base_url: str, api_key: str) -> dict[str, Any]:
        calls.append(f"accounts:{base_url}:{api_key}")
        return {
            "providers": {
                "anthropic": {
                    "account_count": 1,
                    "accounts": [
                        {
                            "email": "anthropic@example.com",
                            "available": True,
                            "failureCount": 0,
                            "planType": None,
                        }
                    ],
                },
                "codex": {"account_count": 0, "accounts": []},
            }
        }

    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setattr(cli, "reload_accounts", fake_reload_accounts)
    monkeypatch.setattr(cli, "get_accounts", fake_get_accounts)

    assert cli.run(["accounts", "--reload"]) == 0

    assert calls == [
        "reload:http://127.0.0.1:8317:sk-test",
        "accounts:http://127.0.0.1:8317:sk-test",
    ]
    output = capsys.readouterr().out
    assert "reloaded accounts" in output
    assert "anthropic@example.com available failures=0" in output


def test_service_install_delegates_custom_host_port(
    tmp_path: Path, monkeypatch: MonkeyPatch
) -> None:
    _write_config(tmp_path)
    called: dict[str, object] = {}

    def fake_install_service(**kwargs) -> Path:
        called.update(kwargs)
        return tmp_path / "pengepul.service"

    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setattr(cli, "install_service", fake_install_service)

    assert cli.run(["service", "install", "--host", "127.0.0.1", "--port", "8318"]) == 0

    assert called["host"] == "127.0.0.1"
    assert called["port"] == 8318
    assert called["start"] is False
    assert called["enable"] is False


def test_pi_path_prints_default_models_path(
    tmp_path: Path, monkeypatch: MonkeyPatch, capsys
) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))

    assert cli.run(["pi", "path"]) == 0

    assert capsys.readouterr().out.strip() == str(tmp_path / ".pi" / "agent" / "models.json")


def test_pi_install_delegates_config_base_url_and_path(
    tmp_path: Path, monkeypatch: MonkeyPatch
) -> None:
    _write_config(tmp_path)
    target = tmp_path / "models.json"
    called: dict[str, object] = {}

    def fake_install_pi_models_config(config, **kwargs) -> Path:
        called["port"] = config.port
        called.update(kwargs)
        return target

    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setattr(cli, "install_pi_models_config", fake_install_pi_models_config)

    assert (
        cli.run(
            [
                "pi",
                "install",
                "--base-url",
                "http://pengepul.example:8317",
                "--path",
                str(target),
                "--web-search",
            ]
        )
        == 0
    )

    assert called == {
        "port": 8317,
        "config_path": None,
        "base_url": "http://pengepul.example:8317",
        "target_path": target,
        "web_search": True,
    }
