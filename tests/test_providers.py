from __future__ import annotations

from pathlib import Path

from pengepul.providers import build_registry
from pengepul.translate import resolve_model


def test_registry_routes_only_anthropic_and_codex(tmp_path: Path) -> None:
    registry = build_registry(str(tmp_path))

    assert [provider.id for provider in registry.all()] == ["anthropic", "codex"]
    assert registry.for_model("claude-sonnet-4-6").id == "anthropic"
    assert registry.for_model("sonnet").id == "anthropic"
    assert registry.for_model("gpt-5").id == "codex"
    assert registry.for_model("gpt-5.4-mini").id == "codex"
    assert registry.for_model("o4-mini").id == "codex"
    assert registry.for_model("codex-mini-latest").id == "codex"
    assert registry.for_model("gpt-4o").id == "anthropic"
    assert registry.for_model("custom-model").id == "anthropic"


def test_resolve_model_aliases() -> None:
    assert resolve_model("opus") == "claude-opus-4-7"
    assert resolve_model("sonnet") == "claude-sonnet-4-6"
    assert resolve_model("haiku") == "claude-haiku-4-5-20251001"
    assert resolve_model("gpt-5.4") == "gpt-5.4"
