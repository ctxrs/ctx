#!/usr/bin/env python3
"""Enforce one target-aware 20k production-CLOC limit per Rust crate."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from typing import Any


POLICY_PATH = "scripts/check-crate-loc-policy-v1.json"
SHA256 = re.compile(r"^[0-9a-f]{64}$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")


class GateError(RuntimeError):
    pass


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
        return root, False
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode == 0:
        return Path(result.stdout.decode("utf-8").strip()).resolve(), True
    raise GateError("could not locate repository root")


def normalized_path(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise GateError(f"{label} must be a nonempty path")
    if any(character in value for character in ("\x00", "\t", "\n", "\r", "\\")):
        raise GateError(f"{label} is not normalized: {value!r}")
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


def read_policy(root: Path, has_git: bool) -> tuple[dict[str, Any], dict[str, Any]]:
    configured = os.environ.get("CTX_CRATE_LOC_POLICY_FILE", POLICY_PATH)
    path = Path(configured)
    if not path.is_absolute():
        path = root / path
    path = path.absolute()
    try:
        relative = path.relative_to(root).as_posix()
    except ValueError as error:
        raise GateError("crate LOC policy must be inside the repository") from error
    if has_git and git(root, "ls-files", "--error-unmatch", relative, check=False).returncode != 0:
        raise GateError("crate LOC policy must be tracked in git")
    policy = load_json(path, "crate LOC policy")
    expected = {
        "schema_version",
        "policy",
        "metric_policy",
        "hard_limit",
        "grandfathered_at",
        "grandfathered",
    }
    if set(policy) != expected or policy.get("schema_version") != 1:
        raise GateError("crate LOC policy schema is unsupported")
    if not isinstance(policy.get("policy"), str) or not policy["policy"].strip():
        raise GateError("crate LOC policy rationale must be nonempty")
    if policy.get("hard_limit") != 20_000:
        raise GateError("crate LOC hard limit must be exactly 20000")
    metric_path = root / normalized_path(policy.get("metric_policy"), "metric_policy")
    metric_policy = load_json(metric_path, "LOC metric policy")
    metric = metric_policy.get("metric")
    if not isinstance(metric, dict):
        raise GateError("LOC metric policy omits metric")
    return policy, metric


def validate_ledger(policy: dict[str, Any]) -> tuple[str, list[dict[str, Any]]]:
    snapshot = policy.get("grandfathered_at")
    if not isinstance(snapshot, str) or COMMIT.fullmatch(snapshot) is None:
        raise GateError("grandfathered_at must be a full lowercase commit SHA")
    entries = policy.get("grandfathered")
    if not isinstance(entries, list):
        raise GateError("grandfathered entries must be an array")
    normalized: list[dict[str, Any]] = []
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"package", "manifest", "code_baseline"}:
            raise GateError("grandfathered entry is malformed")
        package = entry["package"]
        if not isinstance(package, str) or not package:
            raise GateError("grandfathered package must be nonempty")
        manifest = normalized_path(entry["manifest"], f"manifest for {package}")
        baseline = entry["code_baseline"]
        if not isinstance(baseline, int) or isinstance(baseline, bool) or baseline <= policy["hard_limit"]:
            raise GateError(f"grandfathered baseline must exceed 20000: {package}")
        normalized.append({"package": package, "manifest": manifest, "code_baseline": baseline})
    keys = [(entry["package"], entry["manifest"]) for entry in normalized]
    if keys != sorted(keys):
        raise GateError("grandfathered entries must be sorted by package and manifest")
    if len(keys) != len(set(keys)):
        raise GateError("grandfathered packages and manifests must be unique")
    return snapshot, normalized


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


def verify_scc(scc: Path, metric: dict[str, Any]) -> None:
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


def load_toml(path: Path, label: str) -> dict[str, Any]:
    try:
        value = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise GateError(f"{label} is not valid UTF-8 TOML: {error}") from error
    if not isinstance(value, dict):
        raise GateError(f"{label} root must be a table")
    return value


def workspace_packages(root: Path) -> list[dict[str, str]]:
    cargo = load_toml(root / "Cargo.toml", "workspace Cargo.toml")
    workspace = cargo.get("workspace")
    if not isinstance(workspace, dict) or not isinstance(workspace.get("members"), list):
        raise GateError("workspace Cargo.toml must declare members")
    manifests: list[Path] = []
    for member in workspace["members"]:
        member = normalized_path(member, "workspace member")
        matches = sorted(root.glob(member))
        if not matches:
            raise GateError(f"workspace member does not exist: {member}")
        manifests.extend(path / "Cargo.toml" for path in matches)
    packages: list[dict[str, str]] = []
    for manifest in sorted(set(manifests)):
        if not manifest.is_file():
            raise GateError(f"workspace package manifest is missing: {manifest.relative_to(root)}")
        cargo = load_toml(manifest, str(manifest.relative_to(root)))
        package = cargo.get("package")
        if not isinstance(package, dict) or not isinstance(package.get("name"), str):
            raise GateError(f"workspace package omits package.name: {manifest.relative_to(root)}")
        packages.append(
            {
                "package": package["name"],
                "manifest": manifest.relative_to(root).as_posix(),
                "root": manifest.parent.relative_to(root).as_posix(),
                "build": package.get("build", "build.rs"),
            }
        )
    names = [item["package"] for item in packages]
    if len(names) != len(set(names)):
        raise GateError("workspace package names must be unique")
    return sorted(packages, key=lambda item: item["package"])


def source_inventory(root: Path, has_git: bool) -> set[str]:
    if has_git:
        raw = git(root, "ls-files", "-z", "--cached", "--others", "--exclude-standard").stdout.split(b"\0")
    else:
        value = os.environ.get("CTX_CRATE_LOC_PATHS_MANIFEST") or os.environ.get("CTX_LOC_PATHS_MANIFEST")
        if not value:
            raise GateError("sandboxed crate LOC gate requires CTX_CRATE_LOC_PATHS_MANIFEST")
        manifest = Path(value)
        if not manifest.is_absolute() or not manifest.is_file():
            raise GateError("crate LOC source manifest must be an absolute file")
        raw = manifest.read_bytes().splitlines()
    paths: set[str] = set()
    for item in raw:
        if not item:
            continue
        try:
            path = item.decode("utf-8")
        except UnicodeDecodeError as error:
            raise GateError("crate LOC source paths must be UTF-8") from error
        normalized_path(path, "source path")
        if (root / path).is_file():
            paths.add(path)
    return paths


def revision_inventory(root: Path, revision: str, package_root: str) -> set[str]:
    result = git(root, "ls-tree", "-r", "--name-only", "-z", revision, package_root)
    paths: set[str] = set()
    for item in result.stdout.split(b"\0"):
        if not item:
            continue
        try:
            path = item.decode("utf-8")
        except UnicodeDecodeError as error:
            raise GateError("snapshot source paths must be UTF-8") from error
        normalized_path(path, "snapshot source path")
        paths.add(path)
    return paths


def is_test_only_source(relative: PurePosixPath) -> bool:
    parts = relative.parts
    base = relative.name
    if any(
        part in {"test", "tests", "test_support"}
        or part.endswith("_tests")
        or part.startswith("test_support")
        for part in parts[:-1]
    ):
        return True
    return base == "tests.rs" or base.endswith("_tests.rs") or base.startswith("test_support")


def package_sources(root: Path, package: dict[str, str], inventory: set[str]) -> list[str]:
    package_root = PurePosixPath(package["root"])
    source_root = package_root / "src"
    paths: list[str] = []
    for path in inventory:
        pure = PurePosixPath(path)
        if pure.suffix != ".rs" or source_root not in pure.parents:
            continue
        relative = pure.relative_to(package_root)
        if not is_test_only_source(relative):
            paths.append(path)
    build = package["build"]
    if build is not False:
        if not isinstance(build, str):
            raise GateError(f"package.build must be a path or false: {package['package']}")
        build_path = (package_root / normalized_path(build, f"build script for {package['package']}")).as_posix()
        if build_path in inventory:
            paths.append(build_path)
        elif build != "build.rs":
            raise GateError(f"declared build script is missing: {build_path}")
    return sorted(set(paths))


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
        missing = sorted(set(paths) - set(counts))
        unexpected = sorted(set(counts) - set(paths))
        raise GateError(f"scc report/source mismatch: missing={missing}, unexpected={unexpected}")
    return sum(counts.values()), counts


def revision_package_count(root: Path, scc: Path, revision: str, package: dict[str, str]) -> int:
    paths = package_sources(root, package, revision_inventory(root, revision, package["root"]))
    with tempfile.TemporaryDirectory(prefix="ctx-crate-loc-") as temporary:
        temporary_root = Path(temporary)
        for path in paths:
            blob = git(root, "show", f"{revision}:{path}", check=False)
            if blob.returncode != 0:
                raise GateError(f"source path does not exist at {revision}: {path}")
            destination = temporary_root / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(blob.stdout)
        return run_scc(scc, temporary_root, paths)[0]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args()
    root, has_git = repo_context()
    policy, metric = read_policy(root, has_git)
    snapshot, ledger = validate_ledger(policy)
    scc = find_scc(root)
    verify_scc(scc, metric)
    packages = workspace_packages(root)
    inventory = source_inventory(root, has_git)
    ledger_by_package = {entry["package"]: entry for entry in ledger}
    package_by_name = {package["package"]: package for package in packages}
    for entry in ledger:
        package = package_by_name.get(entry["package"])
        if package is None or package["manifest"] != entry["manifest"]:
            raise GateError(f"stale grandfathered package: {entry['package']}")

    results: list[dict[str, Any]] = []
    failures: list[str] = []
    for package in packages:
        paths = package_sources(root, package, inventory)
        source_prefix = f"{package['root']}/src/"
        if not any(path.startswith(source_prefix) for path in paths):
            raise GateError(
                f"workspace package has no declared production Rust sources: {package['package']}"
            )
        code, _ = run_scc(scc, root, paths)
        entry = ledger_by_package.get(package["package"])
        status = "pass"
        ceiling = policy["hard_limit"]
        if entry is not None:
            ceiling = entry["code_baseline"]
            status = "grandfathered"
            if code <= policy["hard_limit"]:
                failures.append(f"{package['package']}: stale grandfathered entry at {code} CLOC")
            elif code > ceiling:
                failures.append(f"{package['package']}: {code} CLOC > no-growth ceiling {ceiling}")
            elif code < ceiling:
                failures.append(
                    f"{package['package']}: stale no-growth ceiling {ceiling}; lower it to current {code} CLOC"
                )
        elif code > policy["hard_limit"]:
            status = "fail"
            failures.append(f"{package['package']}: {code} CLOC > hard limit {policy['hard_limit']}")
        results.append(
            {
                "package": package["package"],
                "manifest": package["manifest"],
                "production_cloc": code,
                "production_files": len(paths),
                "ceiling": ceiling,
                "status": status,
            }
        )

    if has_git and ledger:
        resolved = git(root, "rev-parse", "--verify", f"{snapshot}^{{commit}}", check=False)
        if resolved.returncode != 0:
            raise GateError(f"grandfathered_at is not a commit: {snapshot}")
        for entry in ledger:
            package = package_by_name[entry["package"]]
            snapshot_count = revision_package_count(root, scc, snapshot, package)
            if snapshot_count != entry["code_baseline"]:
                raise GateError(
                    f"grandfathered baseline mismatch at {snapshot}: {entry['package']} "
                    f"was {snapshot_count}, policy says {entry['code_baseline']}"
                )

    report = {
        "schema_version": 1,
        "metric": {"tool": "scc", "version": metric["version"], "field": "Code"},
        "hard_limit": policy["hard_limit"],
        "packages": results,
    }
    print(json.dumps(report, sort_keys=True, separators=(",", ":")))
    if failures:
        print("crate LOC gate failed:", file=sys.stderr)
        for failure in sorted(failures):
            print(f"  {failure}", file=sys.stderr)
        return 1
    print(
        f"crate LOC gate passed ({len(results)} crates, {len(ledger)} temporary no-growth entries, "
        f"hard limit {policy['hard_limit']} production CLOC)."
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as error:
        print(f"crate LOC gate failed: {error}", file=sys.stderr)
        raise SystemExit(1) from None
