from __future__ import annotations

import base64
import hashlib
import json
import os
import secrets
import uuid
from collections.abc import Mapping
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from .types import PKCECodes


def now_iso() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def expires_in_iso(seconds: int | float | None, default_seconds: int = 3600) -> str:
    ttl = default_seconds if seconds is None else int(seconds)
    return (datetime.now(timezone.utc) + timedelta(seconds=ttl)).isoformat().replace("+00:00", "Z")


def iso_to_timestamp(value: str | None) -> float:
    if not value:
        return 0.0
    normalized = value.replace("Z", "+00:00")
    return datetime.fromisoformat(normalized).timestamp()


def resolve_auth_dir(path: str) -> str:
    if path.startswith("~"):
        return str(Path.home() / path[2:] if path.startswith("~/") else Path.home() / path[1:])
    return str(Path(path).expanduser().resolve())


def generate_api_key() -> str:
    return "sk-" + secrets.token_hex(32)


def base64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).decode("ascii").rstrip("=")


def generate_pkce_codes() -> PKCECodes:
    verifier = base64url(secrets.token_bytes(96))
    challenge = base64url(hashlib.sha256(verifier.encode("ascii")).digest())
    return PKCECodes(code_verifier=verifier, code_challenge=challenge)


def decode_jwt_payload(token: str) -> dict[str, Any]:
    parts = token.split(".")
    if len(parts) < 2:
        raise ValueError("malformed JWT")
    payload = parts[1]
    payload += "=" * (-len(payload) % 4)
    return json.loads(base64.urlsafe_b64decode(payload.encode("ascii")).decode("utf-8"))


def sanitize_email(email: str) -> str:
    safe = "".join(ch if ch.isalnum() or ch in "@._-" else "_" for ch in email)
    return safe.replace("..", "_")


def extract_api_key(headers: Mapping[str, str | None]) -> str | None:
    auth = headers.get("authorization") or headers.get("Authorization")
    if auth and auth.lower().startswith("bearer "):
        return auth[7:].strip()
    api_key = headers.get("x-api-key") or headers.get("X-Api-Key") or headers.get("X-API-Key")
    return api_key.strip() if api_key else None


def hash_api_key(api_key: str | None) -> str:
    if not api_key:
        return "anonymous"
    return hashlib.sha256(api_key.encode("utf-8")).hexdigest()


def get_device_id(auth_dir: str, email: str) -> str:
    Path(auth_dir).mkdir(parents=True, exist_ok=True, mode=0o700)
    digest = hashlib.sha256(email.encode("utf-8")).hexdigest()[:16]
    path = Path(auth_dir) / f".device-{digest}"
    if path.exists():
        return path.read_text(encoding="utf-8").strip()
    device_id = str(uuid.uuid4())
    path.write_text(device_id, encoding="utf-8")
    os.chmod(path, 0o600)
    return device_id


def short_error(exc: BaseException) -> str:
    cause = getattr(exc, "__cause__", None)
    if cause:
        return f"{type(cause).__name__}: {cause}"
    return str(exc)
