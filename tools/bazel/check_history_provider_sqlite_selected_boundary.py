#!/usr/bin/env python3
"""Exact ownership and dependency boundary for the selected SQLite pack."""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path


EXPECTED_NORMAL = {
    "chrono",
    "ctx-history-capture-model",
    "ctx-history-capture-runtime",
    "ctx-history-core",
    "ctx-history-source-io",
    "ctx-history-source-sqlite",
    "rusqlite",
    "serde",
    "serde_json",
    "sha2",
    "thiserror",
    "uuid",
}
EXPECTED_DEV = {"ctx-history-source-sqlite", "tempfile"}
EXPECTED_FEATURES = {
    "default": [],
    "test-support": ["ctx-history-source-sqlite/test-support"],
}
EXPECTED_NORMAL_SPECS = {
    dependency: {"workspace": True}
    for dependency in {
        "chrono",
        "rusqlite",
        "serde",
        "serde_json",
        "sha2",
        "thiserror",
        "uuid",
    }
} | {
    dependency: {"path": f"../{dependency}"}
    for dependency in EXPECTED_NORMAL
    if dependency.startswith("ctx-")
}
EXPECTED_DEV_SPECS = {
    "ctx-history-source-sqlite": {
        "path": "../ctx-history-source-sqlite",
        "features": ["test-support"],
    },
    "tempfile": {"workspace": True},
}
EXPECTED_INTERNAL_BAZEL = {
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-capture-runtime:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-source-io:lib",
    "//crates/ctx-history-source-sqlite:lib",
}
EXPECTED_TEST_INTERNAL_BAZEL = EXPECTED_INTERNAL_BAZEL - {
    "//crates/ctx-history-source-sqlite:lib"
} | {"//crates/ctx-history-source-sqlite:test_support_lib"}
PROVIDERS = {"firebender", "goose", "kiro", "warp"}
PROVIDER_VARIANTS = {
    "firebender": "Firebender",
    "goose": "Goose",
    "kiro": "KiroCli",
    "warp": "Warp",
}


class BoundaryError(RuntimeError):
    pass


def labels_in_assignment(build: str, name: str) -> set[str]:
    match = re.search(rf"(?ms)^{re.escape(name)}\s*=\s*\[(.*?)^\]", build)
    if match is None:
        raise BoundaryError(f"missing Bazel assignment {name}")
    return set(re.findall(r'"(//crates/ctx-history-[^" ]+)"', match.group(1)))


def validate_manifest(path: Path) -> None:
    manifest = tomllib.loads(path.read_text())
    normal_specs = manifest.get("dependencies", {})
    dev_specs = manifest.get("dev-dependencies", {})
    normal = set(normal_specs)
    dev = set(dev_specs)
    if normal != EXPECTED_NORMAL:
        raise BoundaryError(
            f"selected SQLite Cargo dependencies drifted: missing={sorted(EXPECTED_NORMAL-normal)} "
            f"extra={sorted(normal-EXPECTED_NORMAL)}"
        )
    if dev != EXPECTED_DEV:
        raise BoundaryError("selected SQLite Cargo dev dependencies drifted")
    if normal_specs != EXPECTED_NORMAL_SPECS:
        raise BoundaryError("selected SQLite Cargo dependency specifications drifted")
    if dev_specs != EXPECTED_DEV_SPECS:
        raise BoundaryError("selected SQLite Cargo dev dependency specifications drifted")
    if manifest.get("features") != EXPECTED_FEATURES:
        raise BoundaryError("selected SQLite Cargo features drifted")
    if manifest.get("build-dependencies"):
        raise BoundaryError("selected SQLite Cargo build dependencies are forbidden")
    if manifest.get("target"):
        raise BoundaryError("selected SQLite Cargo target-specific dependencies are forbidden")
    forbidden = {"ctx-history-capture", "ctx-history-index"} & (normal | dev)
    if forbidden:
        raise BoundaryError("selected SQLite pack gained upward dependencies")


def validate_build(path: Path) -> None:
    build = path.read_text()
    if labels_in_assignment(build, "PROVIDER_DEPS") != EXPECTED_INTERNAL_BAZEL:
        raise BoundaryError("selected SQLite Bazel production allowlist drifted")
    if labels_in_assignment(build, "PROVIDER_TEST_DEPS") != EXPECTED_TEST_INTERNAL_BAZEL:
        raise BoundaryError("selected SQLite Bazel test allowlist drifted")
    history_labels = set(re.findall(r'"(//crates/ctx-history-[^" ]+)"', build))
    expected_labels = EXPECTED_INTERNAL_BAZEL | EXPECTED_TEST_INTERNAL_BAZEL
    if history_labels != expected_labels:
        raise BoundaryError("selected SQLite Bazel dependency surface drifted")
    if "//crates/ctx-history-capture:" in build or "//crates/ctx-history-index:" in build:
        raise BoundaryError("selected SQLite Bazel target gained an upward dependency")


def rust_sources(root: Path) -> list[Path]:
    return sorted(path for path in root.rglob("*.rs") if path.is_file())


def compact_rust(source: str) -> str:
    source = re.sub(r"(?s)/\*.*?\*/", "", source)
    source = re.sub(r"(?m)//.*$", "", source)
    return "".join(source.split())


def selected_sqlite_selector_authorities(inventory: Path) -> dict[str, str]:
    source = inventory.read_text()
    authorities: dict[str, str] = {}
    for provider, variant in PROVIDER_VARIANTS.items():
        matches = re.findall(
            rf"sqlite_route!\(\s*{variant}\s*,\s*\"[^\"]+\"\s*,\s*"
            rf"(?:true|false)\s*,\s*(?:true|false)\s*,\s*([A-Za-z][A-Za-z0-9_]*)",
            source,
        )
        if len(matches) != 1:
            raise BoundaryError(
                f"capture route inventory must own exactly one {provider} selector authority"
            )
        authorities[provider] = matches[0]
    return authorities


def expected_capture_facade(authorities: dict[str, str]) -> str:
    return f"""
use super::*;
use crate::provider::source_backed::{{
    executable_route, family::document::CaptureSelectedSqliteBinding,
}};

pub(super) fn register_firebender_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {{
    let driver = ctx_history_providers_sqlite_selected::firebender_source_backed_driver_scoped::<
        CaptureSelectedSqliteBinding,
    >(
        &source.path,
        data_root,
        source_root_lineage.map_or(
            ctx_history_core::SourceAnchorScope::Unqualified,
            ctx_history_core::SourceAnchorScope::Lineage,
        ),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::{authorities["firebender"]},
        driver,
    )?);
    Ok(())
}}

pub(super) fn register_kiro_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {{
    let driver = ctx_history_providers_sqlite_selected::kiro_source_backed_driver_scoped::<
        CaptureSelectedSqliteBinding,
    >(
        &source.path,
        data_root,
        source_root_lineage.map_or(
            ctx_history_core::SourceAnchorScope::Unqualified,
            ctx_history_core::SourceAnchorScope::Lineage,
        ),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::{authorities["kiro"]},
        driver,
    )?);
    Ok(())
}}

pub fn register_warp_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    surface_key: impl Into<String>,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {{
    let driver = ctx_history_providers_sqlite_selected::warp_source_backed_driver_scoped::<
        CaptureSelectedSqliteBinding,
    >(
        &source.path,
        data_root,
        surface_key,
        source_root_lineage.map_or(
            ctx_history_core::SourceAnchorScope::Unqualified,
            ctx_history_core::SourceAnchorScope::Lineage,
        ),
    )
    .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::{authorities["warp"]},
        driver,
    )?);
    Ok(())
}}

pub fn register_goose_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    platform_root: impl Into<std::path::PathBuf>,
    retained_routes: Vec<(std::path::PathBuf, std::path::PathBuf)>,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {{
    let retained_routes = retained_routes
        .into_iter()
        .map(|(database, root)| {{
            ctx_history_providers_sqlite_selected::GooseSourceRoute::exact(database, root)
        }})
        .collect();
    let driver = ctx_history_providers_sqlite_selected::goose_source_backed_driver_scoped::<
        CaptureSelectedSqliteBinding,
    >(
        &source.path,
        data_root,
        platform_root.into(),
        retained_routes,
        source_root_lineage.map_or(
            ctx_history_core::SourceAnchorScope::Unqualified,
            ctx_history_core::SourceAnchorScope::Lineage,
        ),
    )
    .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::{authorities["goose"]},
        driver,
    )?);
    Ok(())
}}
"""


def validate_pack_tree(root: Path) -> None:
    provider_root = root / "src/providers"
    actual = {path.name for path in provider_root.iterdir() if path.is_dir()}
    if actual != PROVIDERS:
        raise BoundaryError(
            f"selected SQLite provider cohort drifted: expected={sorted(PROVIDERS)} actual={sorted(actual)}"
        )
    text = "\n".join(path.read_text() for path in rust_sources(root / "src"))
    if re.search(r"\bctx_history_capture(?!(?:_model|_runtime))", text):
        raise BoundaryError("selected SQLite source references capture authority")
    if "ctx_history_index" in text:
        raise BoundaryError("selected SQLite source references index authority")
    for provider in PROVIDERS:
        if f"pub fn {provider}_source_backed_driver" not in text:
            raise BoundaryError(f"selected SQLite pack does not own {provider} route construction")
    if "trait SelectedSqliteCaptureBinding" not in text:
        raise BoundaryError("selected SQLite pack lost its generic lifecycle port")


def validate_capture(cargo: Path, build: Path, root: Path) -> None:
    manifest = tomllib.loads(cargo.read_text())
    dependency = manifest.get("dependencies", {}).get(
        "ctx-history-providers-sqlite-selected"
    )
    if dependency != {"path": "../ctx-history-providers-sqlite-selected"}:
        raise BoundaryError("composition Cargo façade does not depend on the selected SQLite pack")
    dev_dependency = manifest.get("dev-dependencies", {}).get(
        "ctx-history-providers-sqlite-selected"
    )
    if dev_dependency != {
        "path": "../ctx-history-providers-sqlite-selected",
        "features": ["test-support"],
    }:
        raise BoundaryError("capture test façade does not enable selected SQLite test support")
    build_text = build.read_text()
    if "//crates/ctx-history-providers-sqlite-selected:lib" not in build_text:
        raise BoundaryError("composition Bazel façade does not depend on the selected SQLite pack")
    if (
        "//crates/ctx-history-providers-sqlite-selected:test_support_lib"
        not in build_text
    ):
        raise BoundaryError("capture Bazel tests do not use selected SQLite test support")
    stale = [
        provider
        for provider in PROVIDERS
        if any((root / "src/providers" / provider).rglob("*.rs"))
        or (root / "src/providers" / f"{provider}.rs").exists()
    ]
    if stale:
        raise BoundaryError("composition retains selected SQLite provider bodies: " + ", ".join(stale))
    inventory = root / "src/source_backed/inventory.rs"
    authorities = selected_sqlite_selector_authorities(inventory)
    facade = root / "src/source_backed/registration/families/sqlite/other.rs"
    if compact_rust(facade.read_text()) != compact_rust(expected_capture_facade(authorities)):
        raise BoundaryError(
            "composition selected SQLite registration façade is not the exact thin composition "
            "of pack drivers and inventory-owned selector authorities"
        )
    selected_references = {
        path.relative_to(root).as_posix(): path.read_text().count(
            "ctx_history_providers_sqlite_selected::"
        )
        for path in rust_sources(root / "src")
        if "ctx_history_providers_sqlite_selected::" in path.read_text()
    }
    expected_references = {
        "src/source_backed/family/document.rs": 1,
        "src/source_backed/registration/families/sqlite/other.rs": 5,
    }
    if selected_references != expected_references:
        raise BoundaryError(
            "capture selected SQLite pack references escaped the lifecycle binding and "
            "registration façade"
        )


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("pack_cargo", type=Path)
    parser.add_argument("pack_build", type=Path)
    parser.add_argument("pack_root", type=Path)
    parser.add_argument("capture_cargo", type=Path)
    parser.add_argument("capture_build", type=Path)
    parser.add_argument("capture_root", type=Path)
    args = parser.parse_args(argv)
    try:
        validate_manifest(args.pack_cargo)
        validate_build(args.pack_build)
        validate_pack_tree(args.pack_root)
        validate_capture(args.capture_cargo, args.capture_build, args.capture_root)
    except (BoundaryError, OSError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    print("selected SQLite provider ownership/dependency boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
