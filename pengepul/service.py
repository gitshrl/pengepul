from __future__ import annotations

import os
import platform
import plistlib
import shlex
import shutil
import subprocess
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from subprocess import CompletedProcess
from typing import Literal

SYSTEMD_UNIT_NAME = "pengepul.service"
LAUNCHD_LABEL = "id.gitshrl.pengepul"

ServiceManager = Literal["systemd", "launchd"]
Runner = Callable[[list[str], bool], CompletedProcess[str]]


@dataclass(slots=True)
class ServiceOptions:
    executable: str
    config_path: str | None
    host: str | None
    port: int | None


def build_service_command(options: ServiceOptions) -> list[str]:
    command = [options.executable, "serve"]
    if options.config_path:
        command.extend(["--config", options.config_path])
    if options.host:
        command.extend(["--host", options.host])
    if options.port is not None:
        command.extend(["--port", str(options.port)])
    return command


def render_systemd_unit(options: ServiceOptions) -> str:
    command = " ".join(shlex.quote(part) for part in build_service_command(options))
    return "\n".join(
        [
            "[Unit]",
            f"Description={SYSTEMD_UNIT_NAME} API relay",
            "After=network-online.target",
            "",
            "[Service]",
            "Type=simple",
            "Environment=PYTHONUNBUFFERED=1",
            f"ExecStart={command}",
            "Restart=on-failure",
            "RestartSec=5",
            "",
            "[Install]",
            "WantedBy=default.target",
            "",
        ]
    )


def render_launchd_plist(
    options: ServiceOptions,
    stdout_path: Path,
    stderr_path: Path,
) -> bytes:
    payload = {
        "Label": LAUNCHD_LABEL,
        "ProgramArguments": build_service_command(options),
        "RunAtLoad": True,
        "KeepAlive": True,
        "StandardOutPath": str(stdout_path),
        "StandardErrorPath": str(stderr_path),
    }
    return plistlib.dumps(payload, sort_keys=False)


def install_service(
    *,
    config_path: str | None = None,
    host: str | None = None,
    port: int | None = None,
    start: bool = False,
    enable: bool = False,
    manager: ServiceManager | None = None,
) -> Path:
    executable = resolve_pengepul_executable()
    options = ServiceOptions(
        executable=executable,
        config_path=config_path,
        host=host,
        port=port,
    )
    selected_manager = manager or detect_service_manager()
    home = Path.home()
    if selected_manager == "systemd":
        return install_systemd_service(options, home, _run_command, start=start, enable=enable)
    if enable:
        raise RuntimeError("--enable is only supported for systemd user services")
    return install_launchd_service(options, home, os.getuid(), _run_command, start=start)


def start_service(manager: ServiceManager | None = None) -> int:
    return _control_service("start", manager)


def stop_service(manager: ServiceManager | None = None) -> int:
    return _control_service("stop", manager)


def restart_service(manager: ServiceManager | None = None) -> int:
    return _control_service("restart", manager)


def service_status(manager: ServiceManager | None = None) -> int:
    selected_manager = manager or detect_service_manager()
    if selected_manager == "systemd":
        return _run_command(
            ["systemctl", "--user", "status", "--no-pager", SYSTEMD_UNIT_NAME],
            check=False,
        ).returncode
    return _run_command(["launchctl", "print", _launchd_target()], check=False).returncode


def uninstall_service(manager: ServiceManager | None = None) -> Path:
    selected_manager = manager or detect_service_manager()
    home = Path.home()
    if selected_manager == "systemd":
        path = _systemd_unit_path(home)
        _run_command(["systemctl", "--user", "stop", SYSTEMD_UNIT_NAME], check=False)
        _run_command(["systemctl", "--user", "disable", SYSTEMD_UNIT_NAME], check=False)
        if path.exists():
            path.unlink()
        _run_command(["systemctl", "--user", "daemon-reload"], check=True)
        return path

    path = _launchd_plist_path(home)
    _run_command(["launchctl", "bootout", _launchd_target()], check=False)
    if path.exists():
        path.unlink()
    return path


def install_systemd_service(
    options: ServiceOptions,
    home: Path,
    runner: Runner,
    *,
    start: bool,
    enable: bool,
) -> Path:
    unit_path = _systemd_unit_path(home)
    unit_path.parent.mkdir(parents=True, exist_ok=True)
    unit_path.write_text(render_systemd_unit(options), encoding="utf-8")
    runner(["systemctl", "--user", "daemon-reload"], check=True)
    if enable:
        runner(["systemctl", "--user", "enable", SYSTEMD_UNIT_NAME], check=True)
    if start:
        runner(["systemctl", "--user", "start", SYSTEMD_UNIT_NAME], check=True)
    return unit_path


def install_launchd_service(
    options: ServiceOptions,
    home: Path,
    uid: int,
    runner: Runner,
    *,
    start: bool,
) -> Path:
    plist_path = _launchd_plist_path(home)
    logs_dir = home / ".pengepul" / "logs"
    plist_path.parent.mkdir(parents=True, exist_ok=True)
    logs_dir.mkdir(parents=True, exist_ok=True)
    plist_path.write_bytes(
        render_launchd_plist(
            options,
            stdout_path=logs_dir / "service.log",
            stderr_path=logs_dir / "service.err.log",
        )
    )
    if start:
        runner(["launchctl", "bootstrap", f"gui/{uid}", str(plist_path)], check=True)
    return plist_path


def detect_service_manager() -> ServiceManager:
    system = platform.system()
    if system == "Linux":
        if not shutil.which("systemctl"):
            raise RuntimeError("systemctl is required to install the Linux user service")
        return "systemd"
    if system == "Darwin":
        if not shutil.which("launchctl"):
            raise RuntimeError("launchctl is required to install the macOS user service")
        return "launchd"
    raise RuntimeError(f"service install is not supported on {system}")


def resolve_pengepul_executable() -> str:
    executable = shutil.which("pengepul")
    if executable:
        return executable
    raise RuntimeError("pengepul is not on PATH; install it with uv tool install first")


def _control_service(
    action: Literal["start", "stop", "restart"],
    manager: ServiceManager | None,
) -> int:
    selected_manager = manager or detect_service_manager()
    if selected_manager == "systemd":
        command = ["systemctl", "--user", action, SYSTEMD_UNIT_NAME]
    else:
        launchd_action = "kickstart" if action in ("start", "restart") else "bootout"
        command = ["launchctl", launchd_action, _launchd_target()]
        if action == "restart":
            command = ["launchctl", "kickstart", "-k", _launchd_target()]
    return _run_command(command, check=False).returncode


def _run_command(command: list[str], check: bool) -> CompletedProcess[str]:
    return subprocess.run(command, check=check)


def _systemd_unit_path(home: Path) -> Path:
    return home / ".config" / "systemd" / "user" / SYSTEMD_UNIT_NAME


def _launchd_plist_path(home: Path) -> Path:
    return home / "Library" / "LaunchAgents" / f"{LAUNCHD_LABEL}.plist"


def _launchd_target() -> str:
    return f"gui/{os.getuid()}/{LAUNCHD_LABEL}"
