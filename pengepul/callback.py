from __future__ import annotations

import logging
import time
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs, urlparse


@dataclass(slots=True)
class CallbackResult:
    code: str
    state: str


SUCCESS_HTML = b"""<!doctype html>
<html><body style="font-family:sans-serif;text-align:center;padding-top:80px">
<h1>Login successful</h1>
<p>You can close this tab and return to the terminal.</p>
</body></html>"""

logger = logging.getLogger(__name__)


def wait_for_callback(
    port: int,
    callback_path: str,
    timeout_seconds: int = 300,
) -> CallbackResult:
    result: CallbackResult | None = None
    error: BaseException | None = None

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, format: str, *args: object) -> None:
            return

        def do_GET(self) -> None:  # noqa: N802
            nonlocal result, error
            parsed = urlparse(self.path)
            if parsed.path != callback_path:
                self.send_response(404)
                self.end_headers()
                return
            params = parse_qs(parsed.query)
            oauth_error = params.get("error", [None])[0]
            if oauth_error:
                error = RuntimeError(f"OAuth error: {oauth_error}")
                self.send_response(400)
                self.end_headers()
                self.wfile.write(str(error).encode("utf-8"))
                return
            code = params.get("code", [None])[0]
            state = params.get("state", [None])[0]
            if not code or not state:
                self.send_response(400)
                self.end_headers()
                self.wfile.write(b"missing code or state")
                return
            result = CallbackResult(code=code, state=state)
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.end_headers()
            self.wfile.write(SUCCESS_HTML)

    server = HTTPServer(("127.0.0.1", port), Handler)
    server.timeout = 1
    deadline = time.monotonic() + timeout_seconds
    logger.info("OAuth callback server listening on http://127.0.0.1:%d%s", port, callback_path)
    try:
        while time.monotonic() < deadline:
            server.handle_request()
            if error:
                raise error
            if result:
                return result
    finally:
        server.server_close()
    raise TimeoutError("OAuth callback timeout")
