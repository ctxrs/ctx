#!/usr/bin/env python3
"""Exact dependency and composition boundary for the Claude/Cursor pack."""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path


PACK = "ctx-history-provider-claude-cursor"
FORBIDDEN = {"ctx-history-capture", "ctx-history-index", "ctx-history-index-format", "ctx-history-index-generation", "ctx-history-index-query"}
REQUIRED = {
    "chrono",
    "ctx-history-capture-model",
    "ctx-history-capture-runtime",
    "ctx-history-core",
    "ctx-history-jsonl",
    "ctx-history-provider-runtime",
    "ctx-history-source-io",
    "serde",
    "serde_json",
    "sha2",
    "uuid",
}


class BoundaryError(RuntimeError):
    pass


def has_concrete_capture_binding(source: str, adapter: str) -> bool:
    """Allow rustfmt whitespace while fixing the runtime type and lineage input."""
    return re.search(
        rf"\b{re.escape(adapter)}\s*::\s*<\s*CaptureProviderRuntime\s*,?\s*>\s*"
        r"\(\s*source_root_lineage\s*,?\s*\)",
        source,
    ) is not None


def manifest(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def validate_pack(path: Path, build: Path) -> None:
    data = manifest(path)
    if data.get("package", {}).get("name") != PACK:
        raise BoundaryError("Claude/Cursor package identity drifted")
    dependencies = data.get("dependencies", {})
    if set(dependencies) != REQUIRED:
        raise BoundaryError("Claude/Cursor production dependency inventory drifted")
    for table_name in ("dependencies", "dev-dependencies", "build-dependencies"):
        for alias, spec in data.get(table_name, {}).items():
            package = spec.get("package", alias) if isinstance(spec, dict) else alias
            if package in FORBIDDEN:
                raise BoundaryError(f"Claude/Cursor pack gained forbidden dependency: {package}")
    source = "\n".join(
        item.read_text(encoding="utf-8")
        for item in path.parent.joinpath("src").rglob("*.rs")
        if item.name != "tests.rs"
    )
    for fragment in ("ctx_history_capture::", "ctx_history_index::", "CaptureJsonlRuntime", "IndexCaptureLifecycle", "SourceBackedProviderRegistry"):
        if fragment in source:
            raise BoundaryError(f"Claude/Cursor pack gained capture authority: {fragment}")
    for fragment in (
        "pub fn claude_jsonl_adapter<B>",
        "pub fn claude_jsonl_adapter_for_named_home<B>",
        "pub fn cursor_jsonl_adapter<B>",
        "pub fn cursor_jsonl_adapter_with_source_root_lineage<B>",
        "ProviderJsonlRuntime<B>",
        "CaptureProvider::Claude",
        "CaptureProvider::Cursor",
    ):
        if fragment not in source:
            raise BoundaryError(f"Claude/Cursor provider surface is incomplete: {fragment}")
    build_source = build.read_text(encoding="utf-8")
    for package in FORBIDDEN:
        if f"//crates/{package}:" in build_source:
            raise BoundaryError(f"Claude/Cursor Bazel graph gained {package} authority")


def validate_capture(cargo: Path, build: Path, modules: Path, direct: Path, other: Path, sources: Path) -> None:
    dependency = manifest(cargo).get("dependencies", {}).get(PACK)
    if dependency != {"path": "../ctx-history-provider-claude-cursor"}:
        raise BoundaryError("capture Cargo composition does not depend on Claude/Cursor pack")
    if f'"//crates/{PACK}:lib"' not in build.read_text(encoding="utf-8"):
        raise BoundaryError("capture Bazel composition does not depend on Claude/Cursor pack")
    if "mod claude" in modules.read_text(encoding="utf-8") or "mod cursor" in modules.read_text(encoding="utf-8"):
        raise BoundaryError("capture still owns Claude or Cursor modules")
    if not has_concrete_capture_binding(
        direct.read_text(encoding="utf-8"),
        "claude_jsonl_adapter_for_named_home",
    ):
        raise BoundaryError("capture Claude registration is not bound to its concrete runtime")
    if not has_concrete_capture_binding(
        other.read_text(encoding="utf-8"),
        "cursor_jsonl_adapter_with_source_root_lineage",
    ):
        raise BoundaryError("capture Cursor registration is not bound to its concrete runtime")
    if "ctx_history_provider_claude_cursor" not in sources.read_text(encoding="utf-8"):
        raise BoundaryError("capture Cursor discovery binding drifted")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("pack_manifest", type=Path)
    parser.add_argument("pack_build", type=Path)
    parser.add_argument("capture_manifest", type=Path)
    parser.add_argument("capture_build", type=Path)
    parser.add_argument("modules", type=Path)
    parser.add_argument("direct", type=Path)
    parser.add_argument("other", type=Path)
    parser.add_argument("sources", type=Path)
    args = parser.parse_args(argv)
    try:
        validate_pack(args.pack_manifest, args.pack_build)
        validate_capture(args.capture_manifest, args.capture_build, args.modules, args.direct, args.other, args.sources)
    except (BoundaryError, OSError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    print("ctx-history-provider-claude-cursor dependency/ownership boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
