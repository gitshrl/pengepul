from __future__ import annotations

import argparse
import asyncio
import contextlib
import secrets
import sys
import webbrowser
from urllib.parse import parse_qs, urlparse

from .app import create_app
from .callback import CallbackResult, wait_for_callback
from .config import load_config
from .providers import build_registry
from .types import ProviderId
from .utils import generate_pkce_codes


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(prog="pengepul")
    parser.add_argument("--config", help="path to config YAML")
    parser.add_argument("--login", action="store_true", help="authorize an upstream account")
    parser.add_argument(
        "--provider",
        choices=("anthropic", "codex"),
        default="anthropic",
        help="upstream account provider for --login",
    )
    parser.add_argument("--manual", action="store_true", help="paste OAuth callback manually")
    parser.add_argument("--host", help="override configured bind host")
    parser.add_argument("--port", type=int, help="override configured bind port")
    args = parser.parse_args(argv)

    config = load_config(args.config)
    registry = build_registry(config.auth_dir)
    for provider in registry.all():
        provider.manager.load()

    if args.login:
        asyncio.run(_login(registry, args.provider, args.manual))
        return

    if args.host is not None:
        config.host = args.host
    if args.port is not None:
        config.port = args.port

    import uvicorn

    uvicorn.run(create_app(config, registry), host=config.host or "127.0.0.1", port=config.port)


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


if __name__ == "__main__":
    main(sys.argv[1:])
