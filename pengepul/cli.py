from __future__ import annotations

import argparse
import asyncio
import contextlib
import secrets
import subprocess
import sys
import webbrowser
from argparse import Namespace
from pathlib import Path
from urllib.parse import parse_qs, urlparse

import yaml

from .admin_client import AdminClientError, get_accounts, get_health, reload_accounts
from .app import create_app
from .callback import CallbackResult, wait_for_callback
from .config import Config, load_config, selected_config_path
from .providers import build_registry
from .service import (
    install_service,
    restart_service,
    service_status,
    start_service,
    stop_service,
    uninstall_service,
)
from .types import ProviderId
from .utils import generate_pkce_codes


def main(argv: list[str] | None = None) -> None:
    raise SystemExit(run(argv))


def run(argv: list[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)

    if args.command is None:
        return _run_default_command(args)
    if args.command == "serve":
        return _run_serve_command(args)
    if args.command == "login":
        return _run_login_command(args)
    if args.command == "status":
        return _run_status_command(args)
    if args.command == "accounts":
        return _run_accounts_command(args)
    if args.command == "config":
        return _run_config_command(args)
    if args.command == "service":
        return _run_service_command(args)
    raise SystemExit(f"unknown command: {args.command}")


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="pengepul")
    parser.add_argument("--config", help="path to config YAML")

    subparsers = parser.add_subparsers(dest="command")

    serve = subparsers.add_parser("serve", help="start the API relay")
    serve.add_argument("--config", dest="command_config", help="path to config YAML")
    serve.add_argument("--host", help="override configured bind host")
    serve.add_argument("--port", type=int, help="override configured bind port")

    login = subparsers.add_parser("login", help="authorize an upstream account")
    login.add_argument("--config", dest="command_config", help="path to config YAML")
    login.add_argument(
        "--provider",
        choices=("anthropic", "codex"),
        default="anthropic",
        help="upstream account provider",
    )
    login.add_argument("--manual", action="store_true", help="paste OAuth callback manually")

    status = subparsers.add_parser("status", help="show local server status")
    status.add_argument("--config", dest="command_config", help="path to config YAML")

    accounts = subparsers.add_parser("accounts", help="show loaded provider accounts")
    accounts.add_argument("--config", dest="command_config", help="path to config YAML")
    accounts.add_argument("--reload", action="store_true", help="reload token files before listing")

    config = subparsers.add_parser("config", help="inspect config")
    config_subparsers = config.add_subparsers(dest="config_command", required=True)
    config_subparsers.add_parser("path", help="print config path")
    config_subparsers.add_parser("show", help="print config YAML")
    config_subparsers.add_parser("api-key", help="print the first configured API key")

    service = subparsers.add_parser("service", help="manage the user service")
    service_subparsers = service.add_subparsers(dest="service_command", required=True)
    service_install = service_subparsers.add_parser("install", help="install user service")
    service_install.add_argument("--config", dest="command_config", help="path to config YAML")
    service_install.add_argument("--host", help="persist service bind host")
    service_install.add_argument("--port", type=int, help="persist service bind port")
    service_install.add_argument("--start", action="store_true", help="start service after install")
    service_install.add_argument("--enable", action="store_true", help="enable Linux user service")
    service_subparsers.add_parser("start", help="start service")
    service_subparsers.add_parser("stop", help="stop service")
    service_subparsers.add_parser("restart", help="restart service")
    service_subparsers.add_parser("status", help="show service manager status")
    service_subparsers.add_parser("uninstall", help="remove user service")
    return parser


def _run_default_command(args: Namespace) -> int:
    return _serve(args.config, host=None, port=None)


def _run_serve_command(args: Namespace) -> int:
    return _serve(_args_config_path(args), args.host, args.port)


def _run_login_command(args: Namespace) -> int:
    config = load_config(_args_config_path(args))
    registry = build_registry(config.auth_dir)
    for provider in registry.all():
        provider.manager.load()
    asyncio.run(_login(registry, args.provider, args.manual))
    return 0


def _serve(config_path: str | None, host: str | None, port: int | None) -> int:
    config = load_config(config_path)
    registry = build_registry(config.auth_dir)
    for provider in registry.all():
        provider.manager.load()

    if host is not None:
        config.host = host
    if port is not None:
        config.port = port
    _run_server(config, registry)
    return 0


def _run_server(config: Config, registry) -> None:
    import uvicorn

    uvicorn.run(create_app(config, registry), host=config.host or "127.0.0.1", port=config.port)


def _run_status_command(args: Namespace) -> int:
    config_path = _args_config_path(args)
    config = load_config(config_path)
    base_url = _base_url(config)
    print(f"config: {selected_config_path(config_path)}")
    print(f"url: {base_url}")
    try:
        health = get_health(base_url)
    except AdminClientError as exc:
        print(f"server: unavailable ({exc})")
        return 1

    print(f"server: {health.get('status', 'unknown')}")
    try:
        accounts = get_accounts(base_url, _first_api_key(config))
    except AdminClientError as exc:
        print(f"accounts: unavailable ({exc})")
        return 1
    _print_account_counts(accounts)
    return 0


def _run_accounts_command(args: Namespace) -> int:
    config = load_config(_args_config_path(args))
    base_url = _base_url(config)
    api_key = _first_api_key(config)
    try:
        if args.reload:
            reload_accounts(base_url, api_key)
            print("reloaded accounts")
        accounts = get_accounts(base_url, api_key)
    except AdminClientError as exc:
        print(f"accounts: unavailable ({exc})")
        return 1
    _print_accounts(accounts)
    return 0


def _run_config_command(args: Namespace) -> int:
    config_path = _args_config_path(args)
    path = selected_config_path(config_path)
    if args.config_command == "path":
        print(path)
        return 0
    config = load_config(config_path)
    if args.config_command == "api-key":
        print(_first_api_key(config))
        return 0
    if args.config_command == "show":
        print(yaml.safe_dump(_read_config_payload(path), sort_keys=False).rstrip())
        return 0
    raise SystemExit(f"unknown config command: {args.config_command}")


def _run_service_command(args: Namespace) -> int:
    try:
        if args.service_command == "install":
            path = install_service(
                config_path=_args_config_path(args),
                host=args.host,
                port=args.port,
                start=args.start,
                enable=args.enable,
            )
            print(f"installed service: {path}")
            return 0
        if args.service_command == "start":
            return start_service()
        if args.service_command == "stop":
            return stop_service()
        if args.service_command == "restart":
            return restart_service()
        if args.service_command == "status":
            return service_status()
        if args.service_command == "uninstall":
            path = uninstall_service()
            print(f"removed service: {path}")
            return 0
    except (RuntimeError, subprocess.CalledProcessError) as exc:
        print(f"service: {exc}")
        return 1
    raise SystemExit(f"unknown service command: {args.service_command}")


async def _login(registry, provider_id: ProviderId, manual: bool) -> None:
    provider = registry.get(provider_id)
    state = secrets.token_urlsafe(32)
    pkce = generate_pkce_codes()
    auth_url = provider.build_auth_url(state, pkce)

    print(f"\nOpen this URL to authorize {provider_id}:\n\n{auth_url}\n")
    if not manual:
        with contextlib.suppress(Exception):
            webbrowser.open(auth_url)
        callback = await asyncio.to_thread(
            wait_for_callback,
            provider.oauth.callback_port,
            provider.oauth.callback_path,
        )
    else:
        callback = _manual_callback()

    token = await provider.exchange_code(callback.code, callback.state, state, pkce)
    provider.manager.add_account(token)
    print(f"saved {provider_id} account token for {token.email}")


def _manual_callback() -> CallbackResult:
    value = input("Paste the full callback URL or authorization code: ").strip()
    if value.startswith("http://") or value.startswith("https://"):
        parsed = urlparse(value)
        params = parse_qs(parsed.query)
        code = params.get("code", [None])[0]
        state = params.get("state", [None])[0]
        if not code or not state:
            raise SystemExit("callback URL is missing code or state")
        return CallbackResult(code=code, state=state)
    state = input("Paste returned state: ").strip()
    if not value or not state:
        raise SystemExit("manual login requires code and state")
    return CallbackResult(code=value, state=state)


def _args_config_path(args: Namespace) -> str | None:
    command_config = getattr(args, "command_config", None)
    return command_config or args.config


def _base_url(config: Config) -> str:
    host = config.host or "127.0.0.1"
    if host in ("0.0.0.0", "::"):
        host = "127.0.0.1"
    elif ":" in host and not host.startswith("["):
        host = f"[{host}]"
    return f"http://{host}:{config.port}"


def _first_api_key(config: Config) -> str:
    if not config.api_keys:
        raise SystemExit("config has no API keys")
    return sorted(config.api_keys)[0]


def _print_account_counts(payload: dict[str, object]) -> None:
    providers = _providers_from_payload(payload)
    for provider_id, provider in providers.items():
        count = int(provider.get("account_count") or 0)
        suffix = "account" if count == 1 else "accounts"
        print(f"{provider_id}: {count} {suffix}")


def _print_accounts(payload: dict[str, object]) -> None:
    providers = _providers_from_payload(payload)
    for provider_id, provider in providers.items():
        accounts = provider.get("accounts") or []
        count = int(provider.get("account_count") or 0)
        suffix = "account" if count == 1 else "accounts"
        print(f"{provider_id}: {count} {suffix}")
        if not isinstance(accounts, list):
            continue
        for account in accounts:
            if not isinstance(account, dict):
                continue
            email = str(account.get("email") or "unknown")
            state = "available" if account.get("available") else "unavailable"
            failures = int(account.get("failureCount") or 0)
            line = f"  {email} {state} failures={failures}"
            plan_type = account.get("planType")
            if plan_type:
                line += f" plan={plan_type}"
            print(line)


def _providers_from_payload(payload: dict[str, object]) -> dict[str, dict[str, object]]:
    providers = payload.get("providers")
    if not isinstance(providers, dict):
        return {}
    output: dict[str, dict[str, object]] = {}
    for key, value in providers.items():
        if isinstance(key, str) and isinstance(value, dict):
            output[key] = value
    return output


def _read_config_payload(path: Path) -> dict[str, object]:
    parsed = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    if not isinstance(parsed, dict):
        raise SystemExit(f"{path} must contain a YAML mapping")
    return {str(key): value for key, value in parsed.items()}


if __name__ == "__main__":
    main(sys.argv[1:])
