#!/usr/bin/env python3
"""Validate the static Cargo/Bazel and source-ownership semantic-index seam."""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from collections import Counter
from pathlib import Path
from typing import Any


EXPECTED_FEATURES = {"default": [], "test-support": []}
EXPECTED_DEPENDENCIES: dict[str, Any] = {
    "anyhow": {"workspace": True},
    "ctx-history-core": {"path": "../ctx-history-core"},
    "ctx-history-index": {"path": "../ctx-history-index"},
    "ctx-history-platform": {"path": "../ctx-history-platform"},
    "ctx-semantic-model": {"path": "../ctx-semantic-model"},
    "fs2": "0.4.3",
    "memmap2": {"workspace": True},
    "rusqlite": {"workspace": True},
    "serde": {"workspace": True},
    "serde_json": {"workspace": True},
    "sha2": {"workspace": True},
    "thiserror": {"workspace": True},
    "uuid": {"workspace": True},
}
EXPECTED_DEV_DEPENDENCIES: dict[str, Any] = {"tempfile": {"workspace": True}}
EXPECTED_INTERNAL_LABELS = [
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-index:lib",
    "//crates/ctx-history-platform:lib",
    "//crates/ctx-semantic-model:lib",
]
ALLOWED_CLI_SEMANTIC_SOURCES = {
    path.strip()
    for path in """
daemon.rs
daemon/control.rs
daemon/seam_tests.rs
daemon_autostart.rs
daemon_autostart/autostart.rs
daemon_autostart/handoff.rs
daemon_autostart/handoff/termination.rs
daemon_autostart/handoff/termination/legacy.rs
daemon_autostart/installation.rs
daemon_autostart/recovery.rs
daemon_autostart/tests.rs
daemon_status.rs
daemon_status/render.rs
daemon_status/tests.rs
daemon_supervisor.rs
daemon_supervisor/coordination.rs
daemon_supervisor/environment.rs
daemon_supervisor/report.rs
daemon_supervisor/state.rs
daemon_supervisor/tests.rs
daemon_supervisor/unsupported.rs
daemon_supervisor/windows.rs
daemon_service_ports.rs
health_search.rs
model_config.rs
model_config_tests.rs
paths_status.rs
paths_status/binary_identity.rs
paths_status/tests.rs
query_adapter.rs
query_adapter/tests.rs
query_service.rs
runtime_limits.rs
source_backed_pro_catch_up.rs
source_backed_pro_catch_up/lease_reconciliation.rs
source_backed_pro_catch_up/status.rs
source_backed_pro_catch_up/tests.rs
source_backed_refresh_coordinator.rs
source_status.rs
source_status_tests.rs
tests.rs
tests/lifecycle.rs
tests/locking.rs
""".splitlines()
    if path.strip()
}

ALLOWED_DAEMON_SERVICE_SEMANTIC_SOURCES = {
    path.strip()
    for path in """
daemon.rs
daemon/config_reload.rs
daemon/config_reload/tests.rs
daemon_retry.rs
daemon_scheduler.rs
daemon_scheduler_tests.rs
daemon_worker.rs
daemon_worker_tests.rs
lib.rs
ports.rs
query_service/server/dispatch.rs
query_service_transport_tests.rs
resource_policy.rs
source_backed_refresh_coordinator/restart_recovery_tests.rs
test_support.rs
""".splitlines()
    if path.strip()
}


class BoundaryError(RuntimeError):
    pass


def _extract_string_list(text: str, name: str) -> list[str]:
    match = re.search(rf"(?ms)^{re.escape(name)}\s*=\s*\[(.*?)^\]", text)
    if match is None:
        raise BoundaryError(f"missing Starlark list {name}")
    return re.findall(r'["\']([^"\']+)["\']', match.group(1))


def _assignment_values(text: str, name: str) -> Counter[str]:
    values = re.findall(rf"(?m)^\s*{re.escape(name)}\s*=\s*(.+),\s*$", text)
    return Counter(value.strip() for value in values)


def validate_manifest(path: Path) -> None:
    with path.open("rb") as handle:
        manifest = tomllib.load(handle)
    if manifest.get("features") != EXPECTED_FEATURES:
        raise BoundaryError(
            f"ctx-semantic-index Cargo features drifted: {manifest.get('features')!r}"
        )
    if manifest.get("dependencies") != EXPECTED_DEPENDENCIES:
        raise BoundaryError("ctx-semantic-index Cargo normal dependency inventory drifted")
    if manifest.get("dev-dependencies") != EXPECTED_DEV_DEPENDENCIES:
        raise BoundaryError("ctx-semantic-index Cargo dev dependency inventory drifted")
    unexpected_dependency_tables = sorted(
        key
        for key in manifest
        if key.endswith("dependencies")
        and key not in {"dependencies", "dev-dependencies"}
    )
    if unexpected_dependency_tables or "target" in manifest:
        raise BoundaryError(
            "ctx-semantic-index Cargo dependency table bypass: "
            f"{unexpected_dependency_tables or ['target']}"
        )


def validate_build(path: Path) -> None:
    text = re.sub(r"(?m)#.*$", "", path.read_text(encoding="utf-8"))
    if _extract_string_list(text, "CTX_SEMANTIC_INDEX_DEPS") != EXPECTED_INTERNAL_LABELS:
        raise BoundaryError("ctx-semantic-index Bazel internal dependency inventory drifted")

    expected_deps = Counter(
        {
            "all_crate_deps(normal = True) + CTX_SEMANTIC_INDEX_DEPS": 2,
            "all_crate_deps(normal = True, normal_dev = True) + CTX_SEMANTIC_INDEX_DEPS": 1,
        }
    )
    if _assignment_values(text, "deps") != expected_deps:
        raise BoundaryError("ctx-semantic-index Bazel dependency expressions drifted")

    expected_proc_macro_deps = Counter(
        {
            "all_crate_deps(proc_macro = True)": 2,
            "all_crate_deps(proc_macro = True, proc_macro_dev = True)": 1,
        }
    )
    if _assignment_values(text, "proc_macro_deps") != expected_proc_macro_deps:
        raise BoundaryError("ctx-semantic-index Bazel proc-macro dependency expressions drifted")

    expected_srcs = Counter(
        {
            'glob(["**"], exclude = ["BUILD.bazel"])': 1,
            "PROD_SRCS": 2,
            "RUST_SRCS": 1,
        }
    )
    if _assignment_values(text, "srcs") != expected_srcs:
        raise BoundaryError("ctx-semantic-index Bazel source target inventory drifted")

    expected_flags = Counter(
        {
            "CTX_SEMANTIC_INDEX_RUSTC_FLAGS": 2,
            "CTX_SEMANTIC_INDEX_RUSTC_FLAGS + ['--cfg=feature=\"test-support\"']": 1,
        }
    )
    if _assignment_values(text, "rustc_flags") != expected_flags:
        raise BoundaryError("ctx-semantic-index Bazel feature/cfg inventory drifted")


def validate_cli_partition(repo_root: Path) -> None:
    semantic_root = repo_root / "crates/ctx-cli/src/semantic"
    actual = {
        source.relative_to(semantic_root).as_posix()
        for source in semantic_root.rglob("*.rs")
    }
    violations = sorted(actual - ALLOWED_CLI_SEMANTIC_SOURCES)
    if violations:
        raise BoundaryError(
            "unreviewed semantic source appeared in ctx-cli: " + ", ".join(violations)
        )


def validate_daemon_service_partition(repo_root: Path) -> None:
    service_root = repo_root / "crates/ctx-daemon-service/src"
    semantic_sources = set()
    for source in service_root.rglob("*.rs"):
        text = source.read_text(encoding="utf-8")
        if "ctx_semantic_index::" in text or "ctx_semantic_model::" in text:
            semantic_sources.add(source.relative_to(service_root).as_posix())
    violations = sorted(semantic_sources - ALLOWED_DAEMON_SERVICE_SEMANTIC_SOURCES)
    if violations:
        raise BoundaryError(
            "unreviewed semantic dependency appeared in ctx-daemon-service: "
            + ", ".join(violations)
        )


def validate(repo_root: Path) -> None:
    validate_manifest(repo_root / "crates/ctx-semantic-index/Cargo.toml")
    validate_build(repo_root / "crates/ctx-semantic-index/BUILD.bazel")
    validate_cli_partition(repo_root)
    validate_daemon_service_partition(repo_root)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("repo_root", type=Path)
    args = parser.parse_args()
    try:
        validate(args.repo_root.resolve())
    except (BoundaryError, OSError, tomllib.TOMLDecodeError) as error:
        print(f"semantic-index static boundary check failed: {error}", file=sys.stderr)
        return 1
    print("semantic-index static Cargo/Bazel/source boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
