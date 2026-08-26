#!/usr/bin/env python3
"""Exact dependency and ownership boundary for the Mistral Vibe/Mux pack."""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path
from typing import Any


EXPECTED_DEPENDENCIES: dict[str, Any] = {
    "chrono": {"workspace": True},
    "ctx-history-capture-model": {"path": "../ctx-history-capture-model"},
    "ctx-history-capture-runtime": {"path": "../ctx-history-capture-runtime"},
    "ctx-history-core": {"path": "../ctx-history-core"},
    "ctx-history-jsonl": {"path": "../ctx-history-jsonl"},
    "ctx-history-provider-runtime": {"path": "../ctx-history-provider-runtime"},
    "ctx-history-source-io": {"path": "../ctx-history-source-io"},
    "serde": {"workspace": True},
    "serde_json": {"workspace": True},
    "sha2": {"workspace": True},
}
EXPECTED_DEV_DEPENDENCIES: dict[str, Any] = {
    "ctx-history-jsonl": {
        "path": "../ctx-history-jsonl",
        "features": ["test-support"],
    },
    "tempfile": {"workspace": True},
    "uuid": {"workspace": True},
}
EXPECTED_INTERNAL_BAZEL = {
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-capture-runtime:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-jsonl:lib",
    "//crates/ctx-history-jsonl:test_support_lib",
    "//crates/ctx-history-provider-runtime:lib",
    "//crates/ctx-history-provider-runtime:test_support_lib",
    "//crates/ctx-history-source-io:lib",
    "//crates/ctx-history-source-io:test_support_lib",
}
EXPECTED_SOURCES = {
    "lib.rs",
    "mistral_vibe.rs",
    "mistral_vibe/native_path.rs",
    "mistral_vibe/native_path/source_backed.rs",
    "mistral_vibe/native_path/source_backed/activity.rs",
    "mistral_vibe/native_path/source_backed/tests.rs",
    "mistral_vibe/schema.rs",
    "mistral_vibe/source.rs",
    "mux.rs",
    "mux/metadata.rs",
    "mux/native_path.rs",
    "mux/native_path/source_backed.rs",
    "mux/native_path/source_backed/projection.rs",
    "mux/native_path/source_backed/projection/seam.rs",
    "mux/native_path/source_backed/tests.rs",
    "mux/normalization.rs",
    "mux/source.rs",
}
FORBIDDEN_PACKAGES = {
    "ctx-history-capture",
    "ctx-history-index",
    "ctx-history-index-format",
    "ctx-history-index-generation",
    "ctx-history-index-query",
}


class BoundaryError(RuntimeError):
    pass


def _read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def _dependency_tables(manifest: dict[str, Any]) -> list[dict[str, Any]]:
    tables = [
        manifest.get("dependencies", {}),
        manifest.get("dev-dependencies", {}),
        manifest.get("build-dependencies", {}),
    ]
    for target in manifest.get("target", {}).values():
        tables.extend(
            target.get(name, {})
            for name in ("dependencies", "dev-dependencies", "build-dependencies")
        )
    return [table for table in tables if isinstance(table, dict)]


def validate_manifest(path: Path) -> None:
    with path.open("rb") as handle:
        manifest = tomllib.load(handle)
    if manifest.get("package", {}).get("name") != "ctx-history-provider-mistral-mux":
        raise BoundaryError("Mistral/Mux package identity drifted")
    for table in _dependency_tables(manifest):
        for alias, specification in table.items():
            package = (
                specification.get("package", alias)
                if isinstance(specification, dict)
                else alias
            )
            if package in FORBIDDEN_PACKAGES:
                raise BoundaryError(
                    f"Mistral/Mux pack has forbidden Cargo dependency: {package}"
                )
    if manifest.get("dependencies") != EXPECTED_DEPENDENCIES:
        raise BoundaryError("Mistral/Mux production dependency inventory drifted")
    if manifest.get("dev-dependencies") != EXPECTED_DEV_DEPENDENCIES:
        raise BoundaryError("Mistral/Mux test dependency inventory drifted")
    if manifest.get("build-dependencies") or manifest.get("target"):
        raise BoundaryError("Mistral/Mux dependency-table bypass")


def validate_build(path: Path) -> None:
    source = _read(path)
    internal = set(
        re.findall(r'"(//crates/ctx-history-[^"\s]+:[^"\s]+)"', source)
    )
    if internal != EXPECTED_INTERNAL_BAZEL:
        raise BoundaryError("Mistral/Mux Bazel dependency inventory drifted")
    for package in FORBIDDEN_PACKAGES:
        if f"//crates/{package}:" in source:
            raise BoundaryError(f"Mistral/Mux Bazel graph gained {package} authority")


def validate_sources(manifest: Path) -> None:
    source_root = manifest.parent / "src"
    paths = {
        path.relative_to(source_root).as_posix() for path in source_root.rglob("*.rs")
    }
    if paths != EXPECTED_SOURCES:
        raise BoundaryError(
            "Mistral/Mux source ownership drifted: "
            f"missing={sorted(EXPECTED_SOURCES - paths)} extra={sorted(paths - EXPECTED_SOURCES)}"
        )
    source = "\n".join(_read(source_root / path) for path in sorted(paths))
    required = (
        "pub fn mistral_vibe_jsonl_adapter<B>(",
        "pub fn mistral_vibe_jsonl_adapter_with_source_root_lineage<B>(",
        "pub fn mux_jsonl_adapter<B>()",
        "pub fn mux_jsonl_adapter_with_source_root_lineage<B>(",
        "ProviderJsonlRuntime<B>",
        "ProviderBaseEventLookup<B>",
        "CaptureProvider::MistralVibe",
        "CaptureProvider::Mux",
        "mistral-vibe-content-occurrence-v1",
        "mux-content-occurrence-v1",
    )
    missing = [fragment for fragment in required if fragment not in source]
    if missing:
        raise BoundaryError(
            "Mistral/Mux provider surface is incomplete: " + ", ".join(missing)
        )
    forbidden = (
        "ctx_history_capture::",
        "ctx_history_index::",
        "CaptureJsonlRuntime",
        "jsonl_compat",
        "enum CaptureError",
        "impl JsonlFamilyError",
        "struct IndexCaptureLifecycle",
        "SourceBackedProviderRegistry",
        "native_jsonl",
    )
    retained = [fragment for fragment in forbidden if fragment in source]
    if retained:
        raise BoundaryError(
            "Mistral/Mux pack gained shared/capture authority: "
            + ", ".join(retained)
        )


def validate_capture_composition(
    capture_manifest: Path,
    capture_build: Path,
    provider_modules: Path,
    source_backed: Path,
    registration: Path,
) -> None:
    with capture_manifest.open("rb") as handle:
        manifest = tomllib.load(handle)
    dependency = manifest.get("dependencies", {}).get(
        "ctx-history-provider-mistral-mux"
    )
    if dependency is not None:
        raise BoundaryError("capture Cargo facade regained provider-pack authority")
    build = _read(capture_build)
    if '"//crates/ctx-history-provider-mistral-mux:lib"' in build:
        raise BoundaryError("capture Bazel facade regained provider-pack authority")
    modules = _read(provider_modules)
    if "mod mistral_vibe" in modules or "mod mux" in modules:
        raise BoundaryError("capture still owns Mistral Vibe or Mux provider modules")
    composition = _read(source_backed)
    required_import = (
        "ctx_history_provider_mistral_mux::{\n"
        "    mistral_vibe_jsonl_adapter_with_source_root_lineage, "
        "mux_jsonl_adapter_with_source_root_lineage,\n"
        "}"
    )
    if required_import not in composition:
        raise BoundaryError("capture provider-pack import drifted")
    registration_source = _read(registration)
    required_calls = (
        "mistral_vibe_jsonl_adapter_with_source_root_lineage::<CaptureProviderRuntime>(\n"
        "            source_root_lineage,\n"
        "        )",
        "mux_jsonl_adapter_with_source_root_lineage::<CaptureProviderRuntime>(source_root_lineage)",
    )
    missing = [call for call in required_calls if call not in registration_source]
    if missing:
        raise BoundaryError(
            "capture thin registration drifted: " + ", ".join(missing)
        )


def validate(
    manifest: Path,
    build: Path,
    capture_manifest: Path,
    capture_build: Path,
    provider_modules: Path,
    source_backed: Path,
    registration: Path,
) -> None:
    validate_manifest(manifest)
    validate_build(build)
    validate_sources(manifest)
    validate_capture_composition(
        capture_manifest,
        capture_build,
        provider_modules,
        source_backed,
        registration,
    )


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("build", type=Path)
    parser.add_argument("capture_manifest", type=Path)
    parser.add_argument("capture_build", type=Path)
    parser.add_argument("provider_modules", type=Path)
    parser.add_argument("source_backed", type=Path)
    parser.add_argument("registration", type=Path)
    args = parser.parse_args(argv)
    try:
        validate(**vars(args))
    except (BoundaryError, OSError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    print("ctx-history-provider-mistral-mux dependency/ownership boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
