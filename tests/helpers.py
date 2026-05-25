from __future__ import annotations

import base64
import json
from collections.abc import AsyncIterator

from pengepul.streaming import iter_sse_events


class FakeSSE:
    def __init__(self, chunks: list[bytes]) -> None:
        self._chunks = chunks

    async def aiter_bytes(self) -> AsyncIterator[bytes]:
        for chunk in self._chunks:
            yield chunk


async def collect_sse(chunks: list[bytes]) -> list[tuple[str, str]]:
    return [event async for event in iter_sse_events(FakeSSE(chunks))]


def sse_payload(chunk: str) -> dict[str, object]:
    data = "\n".join(line[6:] for line in chunk.splitlines() if line.startswith("data: "))
    return json.loads(data)


def jwt(payload: dict[str, object]) -> str:
    def encode(value: dict[str, object]) -> str:
        raw = json.dumps(value).encode("utf-8")
        return base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")

    return f"{encode({'alg': 'none'})}.{encode(payload)}."
