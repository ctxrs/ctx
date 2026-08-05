#!/usr/bin/env python3
"""Enforce the target/configuration-aware Rust crate source and graph gate."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import fnmatch
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tomllib
from typing import Any, Iterable

from check_crate_gate_graph import (
    GateError,
    canonical_bytes,
    edge_records,
    empty_report,
    graph_cycles,
    graph_edge_violations,
    hash_value,
    policy_edges,
    source_action_violations,
    validate_policy_packages,
    violation,
)
from check_crate_gate_policy import (
    SNAPSHOT,
    load_snapshot_inventory,
    validate_ledger,
    validate_temporary_edges,
)


POLICY_PATH = "scripts/check-crate-loc-policy-v1.json"
SHA256 = re.compile(r"^[0-9a-f]{64}$")
LABEL = re.compile(r"^//(?:[A-Za-z0-9._+/-]*):[A-Za-z0-9._+-]+$")


@dataclass(frozen=True)
class Platform:
    id: str
    os: str
    arch: str
    env: str
    triple: str
    bazel_platform: str


@dataclass(frozen=True)
class CargoTarget:
    key: str
    kind: str
    name: str
    root: str


@dataclass(frozen=True)
class Token:
    kind: str
    value: str


class SourceView:
    def __init__(self, root: Path, paths: set[str]):
        self.root = root
        self.paths = paths

    def exists(self, path: str) -> bool:
        return path in self.paths and (self.root / path).is_file()

    def read_bytes(self, path: str) -> bytes:
        if not self.exists(path):
            raise GateError(f"declared source is unavailable: {path}")
        try:
            return (self.root / path).read_bytes()
        except OSError as error:
            raise GateError(f"could not read source {path}: {error}") from error

    def read_text(self, path: str, label: str | None = None) -> str:
        try:
            return self.read_bytes(path).decode("utf-8")
        except UnicodeDecodeError as error:
            raise GateError(f"{label or path} is not UTF-8") from error


def emit_report(report: dict[str, Any]) -> None:
    print(canonical_bytes(report).decode("utf-8"))


def git(root: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[bytes]:
    result = subprocess.run(
        ["git", *args],
        cwd=root,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if check and result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise GateError(f"git {' '.join(args)} failed: {detail}")
    return result


def repo_context() -> tuple[Path, bool]:
    configured = os.environ.get("CTX_CRATE_LOC_ROOT") or os.environ.get("CTX_LOC_ROOT")
    if configured:
        root = Path(configured)
        if not root.is_absolute():
            raise GateError("CTX_CRATE_LOC_ROOT must be an absolute path")
        root = root.absolute()
        if not (root / "Cargo.toml").is_file():
            raise GateError("crate LOC root does not contain Cargo.toml")
        has_git = git(root, "rev-parse", "--show-toplevel", check=False).returncode == 0
        return root, has_git
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        raise GateError("could not locate repository root")
    return Path(result.stdout.decode("utf-8").strip()).resolve(), True


def normalized_path(value: Any, label: str, *, allow_glob: bool = False) -> str:
    if not isinstance(value, str) or not value:
        raise GateError(f"{label} must be a nonempty path")
    if any(character in value for character in ("\x00", "\t", "\n", "\r", "\\")):
        raise GateError(f"{label} is not normalized: {value!r}")
    if not allow_glob and any(character in value for character in "*?["):
        raise GateError(f"{label} may not contain a glob: {value}")
    path = PurePosixPath(value)
    if path.is_absolute() or value.endswith("/") or any(part in {"", ".", ".."} for part in path.parts):
        raise GateError(f"{label} is not normalized: {value}")
    return value


def load_json(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise GateError(f"{label} is not valid UTF-8 JSON: {error}") from error
    if not isinstance(value, dict):
        raise GateError(f"{label} root must be an object")
    return value


def repo_file(root: Path, configured: str | None, default: str, label: str) -> Path:
    path = Path(configured or default)
    if not path.is_absolute():
        path = root / path
    path = path.absolute()
    try:
        path.relative_to(root)
    except ValueError as error:
        raise GateError(f"{label} must be inside the repository") from error
    if not path.is_file():
        raise GateError(f"{label} is missing: {path}")
    return path


def read_policy(root: Path) -> tuple[dict[str, Any], dict[str, Any]]:
    policy_path = repo_file(
        root,
        os.environ.get("CTX_CRATE_LOC_POLICY_FILE"),
        POLICY_PATH,
        "crate gate policy",
    )
    policy = load_json(policy_path, "crate gate policy")
    expected = {
        "schema_version",
        "policy",
        "metric_policy",
        "hard_limit",
        "grandfathered_at",
        "grandfathered",
        "packages",
        "expected_edges",
        "temporary_edges",
        "target_inventory",
        "release_targets",
        "snapshot_inventory",
    }
    if set(policy) != expected or policy.get("schema_version") != 2:
        raise GateError("crate gate policy schema is unsupported")
    if not isinstance(policy.get("policy"), str) or not policy["policy"].strip():
        raise GateError("crate gate policy rationale must be nonempty")
    if policy.get("hard_limit") != 20_000:
        raise GateError("crate LOC hard limit must be exactly 20000")
    if policy.get("grandfathered_at") != SNAPSHOT:
        raise GateError(f"grandfathered_at must remain {SNAPSHOT}")
    metric_path = repo_file(
        root,
        normalized_path(policy.get("metric_policy"), "metric_policy"),
        "",
        "LOC metric policy",
    )
    metric_policy = load_json(metric_path, "LOC metric policy")
    metric = metric_policy.get("metric")
    if not isinstance(metric, dict):
        raise GateError("LOC metric policy omits metric")
    return policy, metric


def find_scc(root: Path) -> Path:
    requested = os.environ.get("CTX_CRATE_LOC_SCC") or os.environ.get("CTX_LOC_SCC", "scc")
    candidate = Path(requested)
    if candidate.parent != Path(".") or candidate.is_absolute():
        if not candidate.is_absolute():
            candidate = root / candidate
        resolved = candidate.resolve()
        if not resolved.is_file() or not os.access(resolved, os.X_OK):
            raise GateError(f"pinned scc executable is unavailable: {requested}")
        return resolved
    located = shutil.which(requested)
    if located is None:
        raise GateError("pinned scc executable is unavailable; set CTX_CRATE_LOC_SCC")
    return Path(located).resolve()


def verify_scc(scc: Path, metric: dict[str, Any]) -> dict[str, Any]:
    required = {"tool", "version", "report_field", "archive_sha256", "binary_sha256"}
    if set(metric) != required or metric.get("tool") != "scc" or metric.get("report_field") != "Code":
        raise GateError("LOC metric configuration is malformed")
    for field in ("archive_sha256", "binary_sha256"):
        if not isinstance(metric.get(field), str) or SHA256.fullmatch(metric[field]) is None:
            raise GateError(f"scc {field} pin is malformed")
    actual_hash = hashlib.sha256(scc.read_bytes()).hexdigest()
    if actual_hash != metric["binary_sha256"]:
        raise GateError(f"scc binary hash mismatch: expected {metric['binary_sha256']}, got {actual_hash}")
    result = subprocess.run(
        [str(scc), "--version"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        text=True,
    )
    actual = (result.stdout or result.stderr).strip()
    expected = f"scc version {metric['version']}"
    if result.returncode != 0 or actual != expected:
        raise GateError(f"scc version mismatch: expected {expected}, got {actual!r}")
    value = {
        "tool": "scc",
        "version": metric["version"],
        "field": "Code",
        "binary_sha256": actual_hash,
    }
    value["metric_sha256"] = hash_value(value)
    return value


def source_inventory(root: Path, has_git: bool) -> set[str]:
    configured = os.environ.get("CTX_CRATE_LOC_PATHS_MANIFEST") or os.environ.get("CTX_LOC_PATHS_MANIFEST")
    if configured:
        manifest = Path(configured)
        if not manifest.is_absolute() or not manifest.is_file():
            raise GateError("crate gate source manifest must be an absolute file")
        raw = manifest.read_bytes().splitlines()
    elif has_git:
        raw = git(root, "ls-files", "-z", "--cached", "--others", "--exclude-standard").stdout.split(b"\0")
    else:
        raise GateError("sandboxed crate gate requires CTX_CRATE_LOC_PATHS_MANIFEST")
    paths: set[str] = set()
    for item in raw:
        if not item:
            continue
        try:
            path = item.decode("utf-8")
        except UnicodeDecodeError as error:
            raise GateError("crate gate source paths must be UTF-8") from error
        normalized_path(path, "source path")
        if (root / path).is_file():
            paths.add(path)
    return paths


def load_toml_text(text: str, label: str) -> dict[str, Any]:
    try:
        value = tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        raise GateError(f"{label} is not valid TOML: {error}") from error
    if not isinstance(value, dict):
        raise GateError(f"{label} root must be a table")
    return value


def resolve_relative(base: str, relative: str, label: str) -> str:
    if not isinstance(relative, str) or not relative or "\\" in relative or PurePosixPath(relative).is_absolute():
        raise GateError(f"{label} is not a relative path: {relative!r}")
    parts = list(PurePosixPath(base).parts)
    for part in PurePosixPath(relative).parts:
        if part == "..":
            if not parts:
                raise GateError(f"{label} escapes the repository")
            parts.pop()
        elif part != ".":
            parts.append(part)
    return PurePosixPath(*parts).as_posix()


def dependency_path(base: str, value: Any, workspace_dependencies: dict[str, Any], name: str) -> str | None:
    if not isinstance(value, dict):
        return None
    resolved = value
    if value.get("workspace") is True:
        inherited = workspace_dependencies.get(name)
        if not isinstance(inherited, dict):
            return None
        resolved = inherited
        base = ""
    path = resolved.get("path")
    if not isinstance(path, str):
        return None
    package_dir = resolve_relative(base, path, f"path dependency {name}")
    return (PurePosixPath(package_dir) / "Cargo.toml").as_posix()


def workspace_manifests(view: SourceView) -> list[str]:
    root_cargo = load_toml_text(view.read_text("Cargo.toml"), "workspace Cargo.toml")
    workspace = root_cargo.get("workspace", {})
    if not isinstance(workspace, dict):
        raise GateError("workspace must be a table")
    all_manifests = sorted(path for path in view.paths if path.endswith("/Cargo.toml"))
    selected: set[str] = set()
    if isinstance(root_cargo.get("package"), dict):
        selected.add("Cargo.toml")
    members = workspace.get("members", [])
    if not isinstance(members, list):
        raise GateError("workspace.members must be an array")
    for member in members:
        pattern = normalized_path(member, "workspace member", allow_glob=True)
        matches = [
            manifest
            for manifest in all_manifests
            if fnmatch.fnmatchcase(str(PurePosixPath(manifest).parent), pattern)
        ]
        if not matches:
            raise GateError(f"workspace member does not exist: {member}")
        selected.update(matches)
    excludes = workspace.get("exclude", [])
    if not isinstance(excludes, list):
        raise GateError("workspace.exclude must be an array")
    excluded: set[str] = set()
    for item in excludes:
        pattern = normalized_path(item, "workspace exclude", allow_glob=True)
        excluded.update(
            manifest
            for manifest in all_manifests
            if fnmatch.fnmatchcase(str(PurePosixPath(manifest).parent), pattern)
        )
    selected -= excluded

    workspace_dependencies = workspace.get("dependencies", {})
    if not isinstance(workspace_dependencies, dict):
        workspace_dependencies = {}
    queue = sorted(selected)
    while queue:
        manifest = queue.pop(0)
        cargo = load_toml_text(view.read_text(manifest), manifest)
        base = "" if manifest == "Cargo.toml" else str(PurePosixPath(manifest).parent)
        tables: list[dict[str, Any]] = []
        for key in ("dependencies", "build-dependencies", "dev-dependencies"):
            value = cargo.get(key, {})
            if isinstance(value, dict):
                tables.append(value)
        target = cargo.get("target", {})
        if isinstance(target, dict):
            for target_value in target.values():
                if not isinstance(target_value, dict):
                    continue
                for key in ("dependencies", "build-dependencies", "dev-dependencies"):
                    value = target_value.get(key, {})
                    if isinstance(value, dict):
                        tables.append(value)
        for table in tables:
            for name, value in table.items():
                path = dependency_path(base, value, workspace_dependencies, name)
                if path is None or path in excluded or path in selected:
                    continue
                if path not in view.paths:
                    raise GateError(
                        f"in-workspace path dependency manifest is absent from the source inventory: {path}"
                    )
                selected.add(path)
                queue.append(path)
    return sorted(selected)


def workspace_packages(view: SourceView) -> list[dict[str, Any]]:
    packages: list[dict[str, Any]] = []
    for manifest in workspace_manifests(view):
        cargo = load_toml_text(view.read_text(manifest), manifest)
        package = cargo.get("package")
        if not isinstance(package, dict) or not isinstance(package.get("name"), str):
            raise GateError(f"workspace package omits package.name: {manifest}")
        packages.append(
            {
                "package": package["name"],
                "manifest": manifest,
                "root": "" if manifest == "Cargo.toml" else str(PurePosixPath(manifest).parent),
                "cargo": cargo,
            }
        )
    names = [item["package"] for item in packages]
    manifests = [item["manifest"] for item in packages]
    if len(names) != len(set(names)) or len(manifests) != len(set(manifests)):
        raise GateError("workspace package names and manifests must be unique")
    return sorted(packages, key=lambda item: item["package"])


def target_path(package: dict[str, Any], value: dict[str, Any], default: str, label: str) -> str:
    path = value.get("path", default)
    if not isinstance(path, str):
        raise GateError(f"{label}.path must be a string")
    return (PurePosixPath(package["root"]) / normalized_path(path, f"{label}.path")).as_posix()


def add_target(result: dict[str, CargoTarget], target: CargoTarget) -> None:
    previous = result.get(target.key)
    if previous is not None and previous != target:
        raise GateError(f"duplicate Cargo target identity: {target.key}")
    result[target.key] = target


def auto_targets(view: SourceView, package: dict[str, Any], kind: str) -> list[CargoTarget]:
    root = PurePosixPath(package["root"])
    directory = root / {"bin": "src/bin", "test": "tests", "example": "examples", "bench": "benches"}[kind]
    result: list[CargoTarget] = []
    prefix = directory.as_posix() + "/"
    for path in sorted(view.paths):
        if not path.startswith(prefix) or not path.endswith(".rs"):
            continue
        relative = PurePosixPath(path).relative_to(directory)
        if len(relative.parts) == 1:
            name = relative.stem
        elif len(relative.parts) == 2 and relative.name == "main.rs":
            name = relative.parts[0]
        else:
            continue
        add_key = f"{kind}:{name}"
        result.append(CargoTarget(add_key, kind, name, path))
    return result


def cargo_targets(view: SourceView, package: dict[str, Any]) -> dict[str, CargoTarget]:
    cargo = package["cargo"]
    pkg = cargo["package"]
    result: dict[str, CargoTarget] = {}
    lib = cargo.get("lib")
    default_lib = (PurePosixPath(package["root"]) / "src/lib.rs").as_posix()
    if isinstance(lib, dict) or (pkg.get("autolib", True) is not False and view.exists(default_lib)):
        value = lib if isinstance(lib, dict) else {}
        name = value.get("name", pkg["name"].replace("-", "_"))
        if not isinstance(name, str) or not name:
            raise GateError(f"{package['package']} library name is invalid")
        add_target(result, CargoTarget(f"lib:{name}", "lib", name, target_path(package, value, "src/lib.rs", "lib")))
    table_names = {"bin": "bin", "test": "test", "example": "example", "bench": "bench"}
    for table, kind in table_names.items():
        values = cargo.get(table, [])
        if not isinstance(values, list) or any(not isinstance(item, dict) for item in values):
            raise GateError(f"{package['package']} [[{table}]] entries must be tables")
        for item in values:
            name = item.get("name")
            if not isinstance(name, str) or not name:
                raise GateError(f"{package['package']} [[{table}]] omits name")
            default = {
                "bin": f"src/bin/{name}.rs",
                "test": f"tests/{name}.rs",
                "example": f"examples/{name}.rs",
                "bench": f"benches/{name}.rs",
            }[kind]
            add_target(result, CargoTarget(f"{kind}:{name}", kind, name, target_path(package, item, default, table)))
        auto_key = {"bin": "autobins", "test": "autotests", "example": "autoexamples", "bench": "autobenches"}[kind]
        if pkg.get(auto_key, True) is not False:
            for target in auto_targets(view, package, kind):
                add_target(result, target)
    build = pkg.get("build")
    build_path: str | None = None
    if build is False:
        build_path = None
    elif isinstance(build, str):
        build_path = (PurePosixPath(package["root"]) / normalized_path(build, "package.build")).as_posix()
    elif build is None:
        candidate = (PurePosixPath(package["root"]) / "build.rs").as_posix()
        if view.exists(candidate):
            build_path = candidate
    else:
        raise GateError(f"{package['package']} package.build must be a path or false")
    if build_path is not None:
        add_target(
            result,
            CargoTarget("custom-build:build-script-build", "custom-build", "build-script-build", build_path),
        )
    for target in result.values():
        if not view.exists(target.root):
            raise GateError(f"Cargo target source is missing: {package['package']} {target.key} -> {target.root}")
    return dict(sorted(result.items()))


def target_required_features(package: dict[str, Any], target_key: str) -> set[str]:
    kind, name = target_key.split(":", 1)
    if kind != "bin":
        return set()
    for value in package["cargo"].get("bin", []):
        if value.get("name") == name:
            features = value.get("required-features", [])
            if not isinstance(features, list) or any(not isinstance(item, str) for item in features):
                raise GateError(f"required-features is malformed: {package['package']} {target_key}")
            return set(features)
    return set()


def feature_combinations(features: list[str]) -> list[list[str]]:
    if len(features) > 16:
        raise GateError("production feature powerset exceeds 65536 exact configurations")
    return [
        [feature for index, feature in enumerate(features) if mask & (1 << index)]
        for mask in range(1 << len(features))
    ]


def load_platforms(root: Path, policy: dict[str, Any]) -> list[Platform]:
    path = repo_file(
        root,
        os.environ.get("CTX_CRATE_LOC_RELEASE_TARGETS"),
        normalized_path(policy.get("release_targets"), "release_targets"),
        "release target inventory",
    )
    value = load_json(path, "release target inventory")
    targets = value.get("targets")
    if value.get("schema_version") != 1 or not isinstance(targets, list) or not targets:
        raise GateError("release target inventory is malformed")
    result: list[Platform] = []
    for target in targets:
        if not isinstance(target, dict):
            raise GateError("release target entry must be an object")
        fields = [target.get(key) for key in ("id", "os", "arch", "public_rust_target", "bazel_platform")]
        if any(not isinstance(item, str) or not item for item in fields):
            raise GateError("release target entry omits crate-gate fields")
        env = "gnu" if target["os"] in {"linux", "windows"} else ""
        result.append(Platform(fields[0], fields[1], fields[2], env, fields[3], fields[4]))
    ids = [item.id for item in result]
    if ids != sorted(ids) or len(ids) != len(set(ids)):
        raise GateError("release target inventory must have sorted unique IDs")
    return result


def validate_label(value: Any, label: str) -> str:
    if not isinstance(value, str) or LABEL.fullmatch(value) is None:
        raise GateError(f"{label} is not an exact main-workspace Bazel label")
    return value


def load_target_inventory(root: Path, policy: dict[str, Any]) -> dict[str, Any]:
    path = repo_file(
        root,
        os.environ.get("CTX_CRATE_LOC_TARGET_INVENTORY"),
        normalized_path(policy.get("target_inventory"), "target_inventory"),
        "Rust target inventory",
    )
    value = load_json(path, "Rust target inventory")
    if set(value) != {"schema_version", "packages", "bazel_roots"} or value.get("schema_version") != 2:
        raise GateError("Rust target inventory schema is unsupported")
    if not isinstance(value.get("packages"), dict) or not isinstance(value.get("bazel_roots"), list):
        raise GateError("Rust target inventory envelope is malformed")
    roots = [validate_label(item, "bazel root") for item in value["bazel_roots"]]
    if roots != sorted(roots) or len(roots) != len(set(roots)):
        raise GateError("bazel_roots must be sorted and unique")
    return value


def validate_inventory_packages(
    view: SourceView,
    packages: list[dict[str, Any]],
    inventory: dict[str, Any],
) -> dict[str, dict[str, Any]]:
    declared = inventory["packages"]
    package_names = {item["package"] for item in packages}
    if set(declared) != package_names:
        raise GateError(
            f"Rust target inventory package mismatch: missing={sorted(package_names-set(declared))}, "
            f"stale={sorted(set(declared)-package_names)}"
        )
    label_owners: dict[str, str] = {}
    result: dict[str, dict[str, Any]] = {}
    for package in packages:
        name = package["package"]
        entry = declared[name]
        expected_keys = {
            "manifest",
            "targets",
            "production_targets",
            "test_only_targets",
            "production_features",
            "test_only_features",
            "native_unit",
            "focused_tests",
            "bazel_only_targets",
            "test_only_feature_targets",
            "out_dir_sources",
        }
        if not isinstance(entry, dict) or set(entry) != expected_keys:
            raise GateError(f"Rust target inventory record is malformed: {name}")
        if entry["manifest"] != package["manifest"]:
            raise GateError(f"Rust target inventory manifest mismatch: {name}")
        actual_targets = cargo_targets(view, package)
        targets = entry["targets"]
        if not isinstance(targets, dict) or set(targets) != set(actual_targets):
            raise GateError(f"Rust target inventory target mismatch: {name}")
        for key, label in targets.items():
            validate_label(label, f"target label for {name} {key}")
        features_table = package["cargo"].get("features", {})
        if not isinstance(features_table, dict):
            raise GateError(f"Cargo features table is malformed: {name}")
        production_features = entry["production_features"]
        test_features = entry["test_only_features"]
        for label, values in (("production_features", production_features), ("test_only_features", test_features)):
            if (
                not isinstance(values, list)
                or values != sorted(values)
                or len(values) != len(set(values))
                or any(not isinstance(feature, str) or not feature for feature in values)
            ):
                raise GateError(f"{label} is invalid: {name}")
        if set(production_features) | set(test_features) != set(features_table) or set(production_features) & set(test_features):
            raise GateError(f"Cargo feature partition is incomplete: {name}")
        test_only = entry["test_only_targets"]
        if (
            not isinstance(test_only, list)
            or test_only != sorted(test_only)
            or len(test_only) != len(set(test_only))
            or any(
                key not in actual_targets
                or actual_targets[key].kind != "bin"
                or not target_required_features(package, key)
                or not target_required_features(package, key) <= set(test_features)
                for key in test_only
            )
        ):
            raise GateError(f"test_only_targets is invalid: {name}")
        expected_production = {
            key
            for key, target in actual_targets.items()
            if target.kind in {"lib", "bin", "custom-build"} and key not in test_only
        }
        production = entry["production_targets"]
        if not isinstance(production, dict) or set(production) != expected_production:
            raise GateError(f"production target inventory mismatch: {name}")
        for target_key, variants in production.items():
            if not isinstance(variants, list) or not variants:
                raise GateError(f"production target has no Bazel variants: {name} {target_key}")
            canonical_variants: list[dict[str, Any]] = []
            for variant in variants:
                if not isinstance(variant, dict) or set(variant) != {"label", "features", "kind"}:
                    raise GateError(f"production target variant is malformed: {name} {target_key}")
                expected_kind = "build-script-contract" if actual_targets[target_key].kind == "custom-build" else "rust"
                if variant["kind"] != expected_kind:
                    raise GateError(f"production target variant kind mismatch: {name} {target_key}")
                label = validate_label(variant["label"], f"production label for {name} {target_key}")
                features = variant["features"]
                if (
                    not isinstance(features, list)
                    or features != sorted(features)
                    or len(features) != len(set(features))
                    or any(not isinstance(feature, str) or not feature for feature in features)
                ):
                    raise GateError(f"production feature set is invalid: {name} {target_key}")
                if label in label_owners:
                    raise GateError(
                        f"production Bazel label is not unique: {label} ({label_owners[label]}, {name}:{target_key})"
                    )
                label_owners[label] = f"{name}:{target_key}"
                canonical_variants.append(dict(variant))
            if canonical_variants != sorted(canonical_variants, key=lambda item: (item["features"], item["label"])):
                raise GateError(f"production target variants must be sorted: {name} {target_key}")
        used_features = {
            feature
            for variants in production.values()
            for variant in variants
            for feature in variant["features"]
        }
        if not used_features <= set(production_features):
            raise GateError(f"production target uses a non-production feature: {name}")
        auxiliary: list[str] = []
        if entry["native_unit"] is not None:
            auxiliary.append(validate_label(entry["native_unit"], f"native_unit for {name}"))
        for field in ("focused_tests", "bazel_only_targets"):
            values = entry[field]
            if not isinstance(values, list) or values != sorted(values) or len(values) != len(set(values)):
                raise GateError(f"{field} must be a sorted unique array: {name}")
            auxiliary.extend(validate_label(item, f"{field} label for {name}") for item in values)
        feature_targets = entry["test_only_feature_targets"]
        if not isinstance(feature_targets, dict) or set(feature_targets) != set(test_features):
            raise GateError(f"test-only feature target proof is incomplete: {name}")
        owned_auxiliary = set(targets.values()) | set(auxiliary)
        for feature, values in feature_targets.items():
            if not isinstance(values, list) or not values or values != sorted(values) or len(values) != len(set(values)):
                raise GateError(f"test-only feature target proof is invalid: {name} {feature}")
            validated = {validate_label(item, f"test-only feature label for {name} {feature}") for item in values}
            if not validated <= owned_auxiliary:
                raise GateError(f"test-only feature proof cites an unowned target: {name} {feature}")
        out_dir_sources = entry["out_dir_sources"]
        if not isinstance(out_dir_sources, dict):
            raise GateError(f"out_dir_sources must be an object: {name}")
        for output, source in out_dir_sources.items():
            normalized_path(output, f"OUT_DIR output for {name}")
            if source is not None:
                normalized_path(source, f"OUT_DIR checked-in source for {name}")
                resolved = resolve_relative(package["root"], source, f"OUT_DIR source for {name}")
                if not view.exists(resolved):
                    raise GateError(f"OUT_DIR checked-in source is missing: {resolved}")
        all_labels = list(targets.values()) + auxiliary
        if len(all_labels) != len(set(all_labels)):
            raise GateError(f"Rust target inventory labels are not unique within {name}")
        result[name] = {"entry": entry, "targets": actual_targets, "package": package}
    return result


def lex_rust(source: str, path: str) -> list[Token]:
    tokens: list[Token] = []
    index = 0
    length = len(source)
    while index < length:
        char = source[index]
        if char.isspace():
            index += 1
            continue
        if source.startswith("//", index):
            newline = source.find("\n", index + 2)
            index = length if newline < 0 else newline + 1
            continue
        if source.startswith("/*", index):
            depth = 1
            cursor = index + 2
            while cursor < length and depth:
                if source.startswith("/*", cursor):
                    depth += 1
                    cursor += 2
                elif source.startswith("*/", cursor):
                    depth -= 1
                    cursor += 2
                else:
                    cursor += 1
            if depth:
                raise GateError(f"unterminated Rust comment: {path}")
            index = cursor
            continue
        raw = re.match(r'(?:br|r)(#+)?"', source[index:])
        if raw:
            hashes = raw.group(1) or ""
            start = index + len(raw.group(0))
            marker = '"' + hashes
            end = source.find(marker, start)
            if end < 0:
                raise GateError(f"unterminated raw Rust string: {path}")
            tokens.append(Token("string", source[start:end]))
            index = end + len(marker)
            continue
        if char == '"':
            cursor = index + 1
            escaped = False
            while cursor < length:
                if not escaped and source[cursor] == '"':
                    break
                if not escaped and source[cursor] == "\\":
                    escaped = True
                else:
                    escaped = False
                cursor += 1
            if cursor >= length:
                raise GateError(f"unterminated Rust string: {path}")
            # The parser only needs cfg names and checked-in source paths.  Do
            # not feed Rust escapes (notably ``\u{...}``) to Python's string
            # parser; preserve them while decoding escaped quotes/backslashes.
            value = re.sub(r'\\(["\\])', r'\1', source[index + 1 : cursor])
            tokens.append(Token("string", value))
            index = cursor + 1
            continue
        if char == "'":
            # A lifetime such as `'a` is not a character literal.  Rust char
            # literals are necessarily short; only consume this token when a
            # closing quote appears before whitespace or a structural token.
            cursor = index + 1
            escaped = False
            closing: int | None = None
            while cursor < length and cursor - index <= 10:
                candidate = source[cursor]
                if not escaped and candidate == "'":
                    closing = cursor + 1
                    break
                if candidate in "\r\n":
                    break
                if not escaped and candidate == "\\":
                    escaped = True
                else:
                    escaped = False
                cursor += 1
            if closing is not None:
                index = closing
                continue
            tokens.append(Token("punct", char))
            index += 1
            continue
        match = re.match(r"[A-Za-z_][A-Za-z0-9_]*", source[index:])
        if match:
            tokens.append(Token("ident", match.group(0)))
            index += len(match.group(0))
            continue
        tokens.append(Token("punct", char))
        index += 1
    return tokens


def matching(tokens: list[Token], start: int, opening: str, closing: str) -> int:
    if start >= len(tokens) or tokens[start].kind != "punct" or tokens[start].value != opening:
        raise GateError(f"internal Rust parser error: expected {opening}")
    depth = 1
    index = start + 1
    while index < len(tokens):
        if tokens[index].kind == "punct" and tokens[index].value == opening:
            depth += 1
        elif tokens[index].kind == "punct" and tokens[index].value == closing:
            depth -= 1
            if depth == 0:
                return index
        index += 1
    raise GateError(f"unbalanced Rust delimiter: {opening}")


class CfgParser:
    def __init__(self, tokens: list[Token], platform: Platform, features: set[str]):
        self.tokens = tokens
        self.platform = platform
        self.features = features
        self.index = 0

    def parse(self) -> set[bool]:
        value = self.expression()
        if self.index != len(self.tokens):
            raise GateError("unsupported cfg expression")
        return value

    def expression(self) -> set[bool]:
        if self.index >= len(self.tokens) or self.tokens[self.index].kind != "ident":
            raise GateError("cfg expression must begin with an identifier")
        name = self.tokens[self.index].value
        self.index += 1
        if self.index < len(self.tokens) and self.tokens[self.index].value == "=":
            self.index += 1
            if self.index >= len(self.tokens) or self.tokens[self.index].kind != "string":
                raise GateError("cfg value must be a string")
            value = self.tokens[self.index].value
            self.index += 1
            return self.predicate(name, value)
        if self.index < len(self.tokens) and self.tokens[self.index].value == "(":
            end = matching(self.tokens, self.index, "(", ")")
            inner = self.tokens[self.index + 1 : end]
            self.index = end + 1
            arguments: list[set[bool]] = []
            cursor = 0
            while cursor < len(inner):
                depth = 0
                split = cursor
                while split < len(inner):
                    if inner[split].value == "(":
                        depth += 1
                    elif inner[split].value == ")":
                        depth -= 1
                    elif inner[split].value == "," and depth == 0:
                        break
                    split += 1
                part = inner[cursor:split]
                if part:
                    arguments.append(CfgParser(part, self.platform, self.features).parse())
                cursor = split + 1
            if name == "any":
                return {any(values) for values in _products(arguments)} if arguments else {False}
            if name == "all":
                return {all(values) for values in _products(arguments)} if arguments else {True}
            if name == "not" and len(arguments) == 1:
                return {not value for value in arguments[0]}
            return {False, True}
        return self.predicate(name, None)

    def predicate(self, name: str, value: str | None) -> set[bool]:
        if name == "test" and value is None:
            return {False}
        if name == "unix" and value is None:
            return {self.platform.os in {"linux", "macos", "freebsd"}}
        if name == "windows" and value is None:
            return {self.platform.os == "windows"}
        if name == "feature" and value is not None:
            return {value in self.features}
        known = {
            "target_os": self.platform.os,
            "target_arch": self.platform.arch,
            "target_env": self.platform.env,
            "target_family": "windows" if self.platform.os == "windows" else "unix",
        }
        if name in known and value is not None:
            return {known[name] == value}
        if name == "debug_assertions" and value is None:
            return {False}
        return {False, True}


def _products(values: list[set[bool]]) -> Iterable[tuple[bool, ...]]:
    result: list[tuple[bool, ...]] = [()]
    for options in values:
        result = [prefix + (value,) for prefix in result for value in sorted(options)]
    return result


def attribute_cfg(attribute: list[Token], platform: Platform, features: set[str]) -> set[bool] | None:
    if not attribute or attribute[0].value != "cfg" or len(attribute) < 3 or attribute[1].value != "(":
        return None
    if matching(attribute, 1, "(", ")") != len(attribute) - 1:
        raise GateError("malformed cfg attribute")
    return CfgParser(attribute[2:-1], platform, features).parse()


def split_top_level(tokens: list[Token]) -> list[list[Token]]:
    result: list[list[Token]] = []
    start = 0
    depth = 0
    pairs = {"(": 1, "[": 1, "{": 1, ")": -1, "]": -1, "}": -1}
    for index, token in enumerate(tokens):
        if token.kind == "punct":
            depth += pairs.get(token.value, 0)
            if token.value == "," and depth == 0:
                result.append(tokens[start:index])
                start = index + 1
    result.append(tokens[start:])
    return [value for value in result if value]


def expanded_attributes(
    attributes: list[list[Token]],
    platform: Platform,
    features: set[str],
) -> list[list[Token]]:
    result: list[list[Token]] = []
    queue = list(attributes)
    while queue:
        attribute = queue.pop(0)
        if attribute and attribute[0].value == "cfg_attr" and len(attribute) >= 4 and attribute[1].value == "(":
            if matching(attribute, 1, "(", ")") != len(attribute) - 1:
                raise GateError("malformed cfg_attr attribute")
            parts = split_top_level(attribute[2:-1])
            if len(parts) < 2:
                raise GateError("cfg_attr requires a predicate and attribute")
            if True in CfgParser(parts[0], platform, features).parse():
                queue = parts[1:] + queue
        else:
            result.append(attribute)
    return result


def attributes_enabled(attributes: list[list[Token]], platform: Platform, features: set[str]) -> bool:
    for attribute in expanded_attributes(attributes, platform, features):
        possible = attribute_cfg(attribute, platform, features)
        if possible is not None and True not in possible:
            return False
    return True


def attribute_path(attributes: list[list[Token]], platform: Platform, features: set[str]) -> str | None:
    paths: list[str] = []
    for attribute in expanded_attributes(attributes, platform, features):
        if len(attribute) == 3 and attribute[0].value == "path" and attribute[1].value == "=" and attribute[2].kind == "string":
            paths.append(attribute[2].value)
    if len(paths) > 1:
        raise GateError("multiple active #[path] attributes")
    return paths[0] if paths else None


class RustSourceWalker:
    def __init__(
        self,
        view: SourceView,
        package_root: str,
        platform: Platform,
        features: set[str],
        out_dir_sources: dict[str, str | None],
    ):
        self.view = view
        self.package_root = package_root
        self.platform = platform
        self.features = features
        self.out_dir_sources = out_dir_sources
        self.sources: set[str] = set()
        self.loaded_sources: set[str] = set()
        self.active: set[str] = set()

    def walk(self, root: str) -> tuple[set[str], set[str]]:
        self._file(root, str(PurePosixPath(root).parent))
        return set(self.sources), set(self.loaded_sources)

    def _file(self, path: str, module_dir: str) -> bool:
        if path in self.active:
            return True
        if path in self.loaded_sources:
            return path in self.sources
        if not self.view.exists(path):
            raise GateError(f"reachable Rust source is missing: {path}")
        self.loaded_sources.add(path)
        self.active.add(path)
        tokens = lex_rust(self.view.read_text(path), path)
        contributes = self._scope(tokens, 0, len(tokens), path, module_dir)
        if contributes:
            self.sources.add(path)
        self.active.remove(path)
        return contributes

    def _scope(self, tokens: list[Token], start: int, end: int, source_path: str, module_dir: str) -> bool:
        index = start
        attributes: list[list[Token]] = []
        contributes = False
        while index < end:
            if tokens[index].kind == "punct" and tokens[index].value == "#" and index + 1 < end and tokens[index + 1].kind == "punct" and tokens[index + 1].value == "[":
                close = matching(tokens, index + 1, "[", "]")
                attributes.append(tokens[index + 2 : close])
                index = close + 1
                continue
            if tokens[index].kind == "ident" and tokens[index].value == "mod" and index + 1 < end and tokens[index + 1].kind == "ident":
                name = tokens[index + 1].value
                cursor = index + 2
                while cursor < end and tokens[cursor].value not in {";", "{"}:
                    cursor += 1
                enabled = attributes_enabled(attributes, self.platform, self.features)
                configured_path = attribute_path(attributes, self.platform, self.features)
                attributes = []
                if cursor >= end:
                    raise GateError(f"unterminated mod declaration in {source_path}")
                if tokens[cursor].value == "{":
                    close = matching(tokens, cursor, "{", "}")
                    if enabled:
                        contributes = True
                        self._scope(tokens, cursor + 1, close, source_path, (PurePosixPath(module_dir) / name).as_posix())
                    index = close + 1
                    continue
                if enabled:
                    contributes = True
                    if configured_path is not None:
                        candidate = resolve_relative(
                            str(PurePosixPath(source_path).parent),
                            configured_path,
                            "#[path]",
                        )
                    else:
                        first = (PurePosixPath(module_dir) / f"{name}.rs").as_posix()
                        second = (PurePosixPath(module_dir) / name / "mod.rs").as_posix()
                        available = [path for path in (first, second) if self.view.exists(path)]
                        if len(available) != 1:
                            raise GateError(
                                f"module {name} in {source_path} resolves to {available or 'no checked-in source'}"
                            )
                        candidate = available[0]
                    next_dir = (
                        str(PurePosixPath(candidate).parent)
                        if PurePosixPath(candidate).name == "mod.rs"
                        else (PurePosixPath(candidate).parent / PurePosixPath(candidate).stem).as_posix()
                    )
                    self._file(candidate, next_dir)
                index = cursor + 1
                continue
            if (
                tokens[index].kind == "ident"
                and tokens[index].value == "include"
                and index + 2 < end
                and tokens[index + 1].value == "!"
                and tokens[index + 2].value in {"(", "[", "{"}
            ):
                opening = tokens[index + 2].value
                closing = {"(": ")", "[": "]", "{": "}"}[opening]
                close = matching(tokens, index + 2, opening, closing)
                enabled = attributes_enabled(attributes, self.platform, self.features)
                attributes = []
                if enabled:
                    contributes = True
                    argument = tokens[index + 3 : close]
                    include_path = self._include_path(argument, source_path)
                    if include_path is not None:
                        self._file(include_path, module_dir)
                index = close + 1
                continue
            if tokens[index].kind == "punct" and tokens[index].value == "{":
                close = matching(tokens, index, "{", "}")
                if attributes_enabled(attributes, self.platform, self.features):
                    contributes = True
                    self._scope(tokens, index + 1, close, source_path, module_dir)
                attributes = []
                index = close + 1
                continue
            if tokens[index].kind == "punct" and tokens[index].value == ";":
                attributes = []
            elif attributes_enabled(attributes, self.platform, self.features):
                contributes = True
            index += 1
        return contributes

    def _include_path(self, argument: list[Token], source_path: str) -> str | None:
        if len(argument) == 1 and argument[0].kind == "string":
            return resolve_relative(
                str(PurePosixPath(source_path).parent),
                argument[0].value,
                "include! path",
            )
        text = "".join(token.value for token in argument)
        if "OUT_DIR" in text:
            strings = [
                token.value
                for token in argument
                if token.kind == "string" and token.value != "OUT_DIR"
            ]
            output = "".join(strings).lstrip("/")
            output = normalized_path(output, "OUT_DIR include output")
            if output not in self.out_dir_sources:
                raise GateError(f"OUT_DIR include lacks exact generated/checked-in provenance: {output}")
            source = self.out_dir_sources[output]
            return None if source is None else resolve_relative(self.package_root, source, "OUT_DIR checked-in source")
        if argument and argument[0].value == "concat":
            strings = [
                token.value
                for token in argument
                if token.kind == "string" and token.value != "CARGO_MANIFEST_DIR"
            ]
            if "CARGO_MANIFEST_DIR" in text and strings:
                suffix = "".join(strings)
                suffix = suffix[1:] if suffix.startswith("/") else suffix
                return resolve_relative(self.package_root, suffix, "include! concat path")
        raise GateError(f"unsupported checked-in include! expression in {source_path}")


def target_sources(
    view: SourceView,
    package_root: str,
    target: CargoTarget,
    platform: Platform,
    features: list[str],
    out_dir_sources: dict[str, str | None] | None = None,
) -> set[str]:
    return target_source_inventory(view, package_root, target, platform, features, out_dir_sources)[0]


def target_source_inventory(
    view: SourceView,
    package_root: str,
    target: CargoTarget,
    platform: Platform,
    features: list[str],
    out_dir_sources: dict[str, str | None] | None = None,
) -> tuple[set[str], set[str]]:
    walker = RustSourceWalker(view, package_root, platform, set(features), out_dir_sources or {})
    return walker.walk(target.root)


def run_scc(scc: Path, root: Path, paths: list[str]) -> tuple[int, dict[str, int]]:
    if not paths:
        return 0, {}
    result = subprocess.run(
        [
            str(scc),
            "--ci",
            "--by-file",
            "--format",
            "json",
            "--include-symlinks",
            "--no-cocomo",
            "--no-complexity",
            "--no-gitignore",
            "--no-ignore",
            "--no-scc-ignore",
            *paths,
        ],
        cwd=root,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        raise GateError(f"scc failed: {result.stderr.decode('utf-8', 'replace').strip()}")
    try:
        report = json.loads(result.stdout)
    except (UnicodeError, json.JSONDecodeError) as error:
        raise GateError(f"scc returned malformed JSON: {error}") from error
    counts: dict[str, int] = {}
    if not isinstance(report, list):
        raise GateError("scc JSON report root must be an array")
    for language in report:
        files = language.get("Files") if isinstance(language, dict) else None
        if not isinstance(files, list):
            raise GateError("scc JSON report omits by-file data")
        for item in files:
            if not isinstance(item, dict) or not isinstance(item.get("Location"), str):
                raise GateError("scc JSON file record is malformed")
            path = PurePosixPath(item["Location"]).as_posix()
            if path.startswith("./"):
                path = path[2:]
            code = item.get("Code")
            if path in counts or not isinstance(code, int) or isinstance(code, bool) or code < 0:
                raise GateError(f"scc JSON file record is invalid: {path}")
            counts[path] = code
    if set(counts) != set(paths):
        raise GateError(
            f"scc report/source mismatch: missing={sorted(set(paths)-set(counts))}, "
            f"unexpected={sorted(set(counts)-set(paths))}"
        )
    return sum(counts.values()), counts


def source_digest(view: SourceView, paths: Iterable[str]) -> str:
    records = [
        {"path": path, "sha256": hashlib.sha256(view.read_bytes(path)).hexdigest()}
        for path in sorted(paths)
    ]
    return hash_value(records)


def normalize_bazel_label(value: str) -> str:
    if value.startswith("@@//"):
        return value[2:]
    return value


def parse_bazel_records(paths: list[Path]) -> dict[str, dict[str, Any]]:
    if not paths:
        raise GateError("configured Bazel crate records are required")
    platforms: dict[str, dict[str, Any]] = {}
    for path in paths:
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeError) as error:
            raise GateError(f"could not read configured Bazel crate records: {error}") from error
        if lines != sorted(lines):
            raise GateError(f"configured Bazel crate records are not canonical: {path}")
        platform_id: str | None = None
        crates: dict[str, dict[str, Any]] = {}
        header: tuple[str, str, str] | None = None
        for line in lines:
            fields = line.split("\t")
            if not fields or fields[0] not in {"platform", "crate", "source", "dep"}:
                raise GateError(f"malformed configured Bazel crate record: {line!r}")
            if fields[0] == "platform" and len(fields) == 4:
                if header is not None:
                    raise GateError(f"duplicate platform record in {path}")
                platform_id = fields[1]
                header = (fields[1], fields[2], fields[3])
            elif fields[0] == "crate" and len(fields) == 5:
                label = normalize_bazel_label(fields[1])
                if label in crates:
                    raise GateError(f"duplicate configured Bazel crate: {platform_id} {label}")
                crates[label] = {
                    "crate_name": fields[2],
                    "crate_type": fields[3],
                    "root": fields[4],
                    "sources": set(),
                    "deps": set(),
                }
            elif fields[0] in {"source", "dep"} and len(fields) == 3:
                label = normalize_bazel_label(fields[1])
                if label not in crates:
                    raise GateError(f"configured Bazel detail precedes crate: {line!r}")
                value = normalize_bazel_label(fields[2]) if fields[0] == "dep" else fields[2]
                crates[label]["sources" if fields[0] == "source" else "deps"].add(value)
            else:
                raise GateError(f"malformed configured Bazel crate record: {line!r}")
        if header is None or platform_id is None:
            raise GateError(f"configured Bazel crate records omit platform: {path}")
        if platform_id in platforms:
            raise GateError(f"duplicate configured Bazel platform: {platform_id}")
        platforms[platform_id] = {"triple": header[1], "bazel_platform": header[2], "crates": crates}
    return dict(sorted(platforms.items()))


def configured_record_paths() -> list[Path]:
    value = os.environ.get("CTX_CRATE_LOC_BAZEL_RECORDS", "")
    paths = [Path(item) for item in value.split(os.pathsep) if item]
    if any(not path.is_absolute() or not path.is_file() for path in paths):
        raise GateError("CTX_CRATE_LOC_BAZEL_RECORDS must contain absolute files")
    return paths


def target_predicate_active(predicate: str, platform: Platform) -> bool:
    if predicate == platform.triple:
        return True
    if not predicate.startswith("cfg(") or not predicate.endswith(")"):
        return False
    tokens = lex_rust(predicate[4:-1], f"Cargo target predicate {predicate}")
    return True in CfgParser(tokens, platform, set()).parse()


def workspace_edges(
    view: SourceView,
    packages: list[dict[str, Any]],
    platforms: list[Platform],
) -> dict[str, set[tuple[str, str]]]:
    manifest_to_name = {package["manifest"]: package["package"] for package in packages}
    root_cargo = next((item["cargo"] for item in packages if item["manifest"] == "Cargo.toml"), None)
    if root_cargo is None:
        # A virtual workspace still owns inherited workspace dependencies.
        root_cargo = load_toml_text(view.read_text("Cargo.toml"), "workspace Cargo.toml")
    workspace = root_cargo.get("workspace", {}) if isinstance(root_cargo, dict) else {}
    workspace_dependencies = workspace.get("dependencies", {}) if isinstance(workspace, dict) else {}
    if not isinstance(workspace_dependencies, dict):
        workspace_dependencies = {}
    result = {platform.id: set() for platform in platforms}
    for package in packages:
        cargo = package["cargo"]
        base = package["root"]
        tables: list[tuple[set[str], dict[str, Any]]] = []
        all_platforms = {platform.id for platform in platforms}
        for key in ("dependencies", "build-dependencies"):
            value = cargo.get(key, {})
            if isinstance(value, dict):
                tables.append((all_platforms, value))
        target = cargo.get("target", {})
        if isinstance(target, dict):
            for predicate, target_value in target.items():
                if not isinstance(target_value, dict):
                    continue
                active = {platform.id for platform in platforms if target_predicate_active(predicate, platform)}
                for key in ("dependencies", "build-dependencies"):
                    value = target_value.get(key, {})
                    if isinstance(value, dict):
                        tables.append((active, value))
        for active, table in tables:
            for dependency, value in table.items():
                manifest = dependency_path(base, value, workspace_dependencies, dependency)
                if manifest is None:
                    continue
                target_name = manifest_to_name.get(manifest)
                if target_name is None:
                    continue
                for platform_id in active:
                    result[platform_id].add((package["package"], target_name))
    return result


def main() -> int:
    argparse.ArgumentParser(description=__doc__).parse_args()
    root, has_git = repo_context()
    policy, metric_policy = read_policy(root)
    snapshot = load_snapshot_inventory(root, policy)
    ledger = validate_ledger(policy, snapshot)
    temporary = validate_temporary_edges(policy, snapshot)
    scc = find_scc(root)
    metric = verify_scc(scc, metric_policy)
    paths = source_inventory(root, has_git)
    view = SourceView(root, paths)
    platforms = load_platforms(root, policy)
    inventory = load_target_inventory(root, policy)
    packages = workspace_packages(view)
    inventory_packages = validate_inventory_packages(view, packages, inventory)
    configured = parse_bazel_records(configured_record_paths())
    platform_by_id = {platform.id: platform for platform in platforms}
    if set(configured) != set(platform_by_id):
        raise GateError(
            f"configured Bazel platform mismatch: missing={sorted(set(platform_by_id)-set(configured))}, "
            f"stale={sorted(set(configured)-set(platform_by_id))}"
        )
    for platform_id, record in configured.items():
        expected = platform_by_id[platform_id]
        if record["triple"] != expected.triple or record["bazel_platform"] != expected.bazel_platform:
            raise GateError(f"configured Bazel platform identity mismatch: {platform_id}")

    label_to_variant: dict[str, tuple[str, str, dict[str, Any]]] = {}
    for package_name, item in inventory_packages.items():
        for target_key, variants in item["entry"]["production_targets"].items():
            for variant in variants:
                if variant["kind"] == "rust":
                    label_to_variant[variant["label"]] = (package_name, target_key, variant)

    violations: list[dict[str, Any]] = []
    target_report: list[dict[str, Any]] = []
    package_sources: dict[str, set[str]] = {package["package"]: set() for package in packages}
    package_target_keys: dict[str, list[str]] = {}
    cargo_sources_by_variant: dict[tuple[str, str, str, tuple[str, ...]], set[str]] = {}
    for package_name, item in sorted(inventory_packages.items()):
        entry = item["entry"]
        package = item["package"]
        package_target_keys[package_name] = sorted(entry["production_targets"])
        for target_key, variants in sorted(entry["production_targets"].items()):
            target = item["targets"][target_key]
            variant_reports: list[dict[str, Any]] = []
            cargo_feature_variants = feature_combinations(entry["production_features"])
            for variant in variants:
                variant_union: set[str] = set()
                loaded_union: set[str] = set()
                for platform in platforms:
                    platform_sources: set[str] = set()
                    platform_loaded: set[str] = set()
                    for cargo_features in cargo_feature_variants:
                        sources, loaded = target_source_inventory(
                            view,
                            package["root"],
                            target,
                            platform,
                            cargo_features,
                            entry["out_dir_sources"],
                        )
                        platform_sources.update(sources)
                        platform_loaded.update(loaded)
                    variant_union.update(platform_sources)
                    loaded_union.update(platform_loaded)
                    if variant["kind"] == "rust":
                        cargo_sources_by_variant[(package_name, target_key, platform.id, tuple(variant["features"]))] = platform_loaded
                package_sources[package_name].update(variant_union)
                variant_reports.append(
                    {
                        "label": variant["label"],
                        "features": variant["features"],
                        "cargo_feature_variants": cargo_feature_variants,
                        "kind": variant["kind"],
                        "loaded_source_digest": source_digest(view, loaded_union),
                        "loaded_source_files": len(loaded_union),
                        "source_digest": source_digest(view, variant_union),
                        "source_files": len(variant_union),
                    }
                )
            target_report.append(
                {
                    "package": package_name,
                    "target": target_key,
                    "kind": target.kind,
                    "root": target.root,
                    "variants": variant_reports,
                }
            )

    expected_labels = set(label_to_variant)
    for platform in platforms:
        crates = configured[platform.id]["crates"]
        actual_labels = set(crates)
        for label in sorted(expected_labels - actual_labels):
            violations.append(violation("missing_bazel_target", f"configured Bazel graph omits {label} on {platform.id}", platform=platform.id, label=label))
        for label in sorted(actual_labels - expected_labels):
            violations.append(violation("bazel_only_production_target", f"configured Bazel graph has unowned Rust target {label} on {platform.id}", platform=platform.id, label=label))
        for label in sorted(expected_labels & actual_labels):
            package_name, target_key, variant = label_to_variant[label]
            crate = crates[label]
            target = inventory_packages[package_name]["targets"][target_key]
            if crate["root"] != target.root:
                violations.append(violation("target_root_mismatch", f"Cargo/Bazel target root mismatch for {label} on {platform.id}", platform=platform.id, label=label, cargo=target.root, bazel=crate["root"]))
            cargo_sources = cargo_sources_by_variant[(package_name, target_key, platform.id, tuple(variant["features"]))]
            violations.extend(
                source_action_violations(
                    cargo_sources,
                    crate["sources"],
                    platform=platform.id,
                    label=label,
                )
            )

    all_production_sources = sorted(set().union(*package_sources.values()))
    _total_code, source_cloc = run_scc(scc, root, all_production_sources)
    package_results: list[dict[str, Any]] = []
    ledger_by_package = ledger
    for package in packages:
        name = package["package"]
        sources = sorted(package_sources[name])
        if not sources:
            raise GateError(f"workspace package has no production Rust sources: {name}")
        code = sum(source_cloc[path] for path in sources)
        digest = source_digest(view, sources)
        ceiling = policy["hard_limit"]
        status = "pass"
        entry = ledger_by_package.get(name)
        if entry is not None:
            ceiling = entry["code_baseline"]
            status = "grandfathered"
            if code <= policy["hard_limit"]:
                violations.append(violation("stale_exception", f"{name} no longer needs its migration exception", package=name, production_cloc=code))
            elif code > ceiling:
                violations.append(violation("cloc_growth", f"{name} exceeds its no-growth ceiling", package=name, production_cloc=code, ceiling=ceiling))
            elif code < ceiling:
                violations.append(violation("stale_ceiling", f"{name} shrank; lower its checked-in ceiling", package=name, production_cloc=code, ceiling=ceiling))
        elif code > policy["hard_limit"]:
            status = "fail"
            violations.append(violation("cloc_limit", f"{name} exceeds the hard crate limit", package=name, production_cloc=code, ceiling=policy["hard_limit"]))
        package_results.append(
            {
                "package": name,
                "manifest": package["manifest"],
                "production_cloc": code,
                "production_files": len(sources),
                "source_digest": digest,
                "sources": sources,
                "ceiling": ceiling,
                "status": status,
            }
        )
    for name in sorted(set(ledger_by_package) - {package["package"] for package in packages}):
        violations.append(violation("stale_exception", f"migration exception package no longer exists: {name}", package=name))

    violations.extend(validate_policy_packages(policy, package_results, package_target_keys))
    cargo_edges = workspace_edges(view, packages, platforms)
    expected_edges = policy_edges(policy, platforms)
    violations.extend(graph_edge_violations(cargo_edges, expected_edges))
    expected_union = {edge for values in expected_edges.values() for edge in values}
    for exception_id, edge in temporary.items():
        if edge not in expected_union:
            violations.append(violation("stale_temporary_edge", f"temporary edge record is not in the expected graph: {exception_id}", exception_id=exception_id, source=edge[0], target=edge[1]))

    label_to_package = {label: owner[0] for label, owner in label_to_variant.items()}
    bazel_edges: dict[str, set[tuple[str, str]]] = {platform.id: set() for platform in platforms}
    for platform in platforms:
        for label, crate in configured[platform.id]["crates"].items():
            source_package = label_to_package.get(label)
            if source_package is None:
                continue
            for dep_label in crate["deps"]:
                target_package = label_to_package.get(dep_label)
                if target_package is not None and target_package != source_package:
                    bazel_edges[platform.id].add((source_package, target_package))
    violations.extend(graph_edge_violations(cargo_edges, cargo_edges, bazel_edges))

    cycles = graph_cycles(cargo_edges)
    for cycle in cycles:
        code = "self_loop" if len(cycle["cycle"]) == 2 else "dependency_cycle"
        violations.append(violation(code, "workspace dependency graph contains a cycle", cycle=cycle["cycle"], platforms=cycle["platforms"]))

    violations = sorted(violations, key=lambda item: canonical_bytes(item))
    report = {
        "schema_version": 2,
        "status": "fail" if violations else "pass",
        "metric": metric,
        "platforms": [
            {
                "id": platform.id,
                "triple": platform.triple,
                "bazel_platform": platform.bazel_platform,
            }
            for platform in platforms
        ],
        "targets": target_report,
        "source_digest": hash_value(
            [{"package": item["package"], "source_digest": item["source_digest"]} for item in package_results]
        ),
        "cloc": {"hard_limit": policy["hard_limit"], "packages": package_results},
        "graph": {
            "cargo_edges": edge_records(cargo_edges),
            "bazel_edges": edge_records(bazel_edges),
            "cycles": cycles,
        },
        "violations": violations,
    }
    emit_report(report)
    if violations:
        print(f"Rust crate gate failed with {len(violations)} violation(s).", file=sys.stderr)
        for item in violations:
            print(f"  {item['code']}: {item['detail']}", file=sys.stderr)
        return 1
    print(
        f"Rust crate gate passed ({len(packages)} packages, {len(target_report)} production targets, "
        f"{len(platforms)} release platforms).",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as error:
        report = empty_report(str(error))
        emit_report(report)
        print(f"Rust crate gate failed: {error}", file=sys.stderr)
        raise SystemExit(1) from None
    except Exception as error:  # Keep even malformed future records machine-readable.
        detail = f"internal gate error ({type(error).__name__}): {error}"
        emit_report(empty_report(detail))
        print(f"Rust crate gate failed: {detail}", file=sys.stderr)
        raise SystemExit(1) from None
