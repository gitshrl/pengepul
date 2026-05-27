from __future__ import annotations

import plistlib
from pathlib import Path
from subprocess import CompletedProcess

from pengepul.service import (
    LAUNCHD_LABEL,
    SYSTEMD_UNIT_NAME,
    ServiceOptions,
    install_launchd_service,
    install_systemd_service,
    render_launchd_plist,
    render_systemd_unit,
)


def test_render_systemd_unit_uses_pengepul_serve_with_custom_host_port() -> None:
    unit = render_systemd_unit(
        ServiceOptions(
            executable="/home/dev/.local/bin/pengepul",
            config_path="/home/dev/.pengepul/config.yaml",
            host="127.0.0.1",
            port=8318,
        )
    )

    assert f"Description={SYSTEMD_UNIT_NAME} API relay" in unit
    assert (
        "ExecStart=/home/dev/.local/bin/pengepul serve "
        "--config /home/dev/.pengepul/config.yaml --host 127.0.0.1 --port 8318"
    ) in unit
    assert "Restart=on-failure" in unit


def test_render_launchd_plist_uses_program_arguments() -> None:
    payload = render_launchd_plist(
        ServiceOptions(
            executable="/Users/dev/.local/bin/pengepul",
            config_path="/Users/dev/.pengepul/config.yaml",
            host="127.0.0.1",
            port=8318,
        ),
        stdout_path=Path("/Users/dev/.pengepul/logs/service.log"),
        stderr_path=Path("/Users/dev/.pengepul/logs/service.err.log"),
    )

    plist = plistlib.loads(payload)
    assert plist["Label"] == LAUNCHD_LABEL
    assert plist["ProgramArguments"] == [
        "/Users/dev/.local/bin/pengepul",
        "serve",
        "--config",
        "/Users/dev/.pengepul/config.yaml",
        "--host",
        "127.0.0.1",
        "--port",
        "8318",
    ]
    assert plist["RunAtLoad"] is True
    assert plist["KeepAlive"] is True


def test_install_systemd_service_writes_unit_and_runs_commands(tmp_path: Path) -> None:
    commands: list[list[str]] = []

    def runner(command: list[str], check: bool) -> CompletedProcess[str]:
        commands.append(command)
        return CompletedProcess(command, 0)

    path = install_systemd_service(
        ServiceOptions(
            executable="/home/dev/.local/bin/pengepul",
            config_path=None,
            host="127.0.0.1",
            port=8318,
        ),
        home=tmp_path,
        runner=runner,
        start=True,
        enable=True,
    )

    assert path == tmp_path / ".config" / "systemd" / "user" / SYSTEMD_UNIT_NAME
    assert "ExecStart=/home/dev/.local/bin/pengepul serve --host 127.0.0.1 --port 8318" in (
        path.read_text(encoding="utf-8")
    )
    assert commands == [
        ["systemctl", "--user", "daemon-reload"],
        ["systemctl", "--user", "enable", SYSTEMD_UNIT_NAME],
        ["systemctl", "--user", "start", SYSTEMD_UNIT_NAME],
    ]


def test_install_launchd_service_writes_plist_and_bootstraps_when_started(tmp_path: Path) -> None:
    commands: list[list[str]] = []

    def runner(command: list[str], check: bool) -> CompletedProcess[str]:
        commands.append(command)
        return CompletedProcess(command, 0)

    path = install_launchd_service(
        ServiceOptions(
            executable="/Users/dev/.local/bin/pengepul",
            config_path=None,
            host=None,
            port=None,
        ),
        home=tmp_path,
        uid=501,
        runner=runner,
        start=True,
    )

    assert path == tmp_path / "Library" / "LaunchAgents" / f"{LAUNCHD_LABEL}.plist"
    plist = plistlib.loads(path.read_bytes())
    assert plist["ProgramArguments"] == ["/Users/dev/.local/bin/pengepul", "serve"]
    assert commands == [["launchctl", "bootstrap", "gui/501", str(path)]]
