#!/usr/bin/env python3
"""Enforce the checked-in LOC-v2 CLOC policy with a shrink-only baseline."""

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
from typing import Any


POLICY_PATH = "scripts/check-loc-policy-v2.json"
SOURCE_EXTENSIONS = {
    ".bash",
    ".bzl",
    ".c",
    ".cc",
    ".cjs",
    ".cpp",
    ".cs",
    ".cxx",
    ".go",
    ".h",
    ".hh",
    ".hpp",
    ".hxx",
    ".java",
    ".js",
    ".jsx",
    ".kt",
    ".kts",
    ".mjs",
    ".ps1",
    ".psm1",
    ".py",
    ".rs",
    ".sh",
    ".swift",
    ".ts",
    ".tsx",
}
SOURCE_NAMES = {"BUILD", "BUILD.bazel", "MODULE.bazel", "WORKSPACE", "WORKSPACE.bazel"}
EXCLUDED_COMPONENTS = {"data", "docs", "fixture", "fixtures", "gen", "generated"}
TEST_COMPONENTS = {"test", "tests", "Tests", "__tests__", "test_support"}
DOC_NAMES = {"LICENSE", "NOTICE", "README", "SECURITY.md"}
DOC_SUFFIXES = {
    ".json",
    ".jsonl",
    ".lock",
    ".markdown",
    ".md",
    ".rst",
    ".toml",
    ".txt",
    ".yaml",
    ".yml",
}
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
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode == 0:
        return Path(result.stdout.decode("utf-8").strip()).resolve(), True
    candidates = [Path.cwd(), Path(__file__).resolve().parent.parent]
    for candidate in candidates:
        if (candidate / "Cargo.toml").is_file():
            return candidate.resolve(), False
    raise GateError("could not locate repository root")


def normalized_repo_path(value: Any) -> str:
    if not isinstance(value, str) or not value:
        raise GateError("grandfathered path must be a nonempty string")
    if any(character in value for character in ("\x00", "\t", "\n", "\r", "\\")):
        raise GateError(f"grandfathered path is not normalized: {value!r}")
    path = PurePosixPath(value)
    if path.is_absolute() or value.endswith("/") or any(part in {"", ".", ".."} for part in path.parts):
        raise GateError(f"grandfathered path is not normalized: {value}")
    if any(character in value for character in ("*", "?", "[", "]")):
        raise GateError(f"grandfathered path must be exact, not a pattern: {value}")
    return value


def classify(path: str) -> str | None:
    pure = PurePosixPath(path)
    parts = pure.parts
    base = pure.name

    if any(part in EXCLUDED_COMPONENTS for part in parts[:-1]):
        return None
    if base in {"Cargo.lock", "MODULE.bazel.lock", "package-lock.json"} or base.endswith(".lock"):
        return None
    if base in DOC_NAMES or base.startswith(("README.", "CHANGELOG", "CHANGELOG.")):
        return None
    if pure.suffix in DOC_SUFFIXES:
        return None
    if base not in SOURCE_NAMES and pure.suffix not in SOURCE_EXTENSIONS:
        return None

    parent_parts = parts[:-1]
    is_test = any(part in TEST_COMPONENTS for part in parent_parts)
    is_test = is_test or any(
        parent_parts[index : index + 2] == ("src", "test")
        for index in range(max(0, len(parent_parts) - 1))
    )
    is_test = is_test or base == "tests.rs" or base.startswith("test_support")
    is_test = is_test or re.search(r"_(?:test|tests)\.[^.]+$", base) is not None
    is_test = is_test or re.search(r"\.(?:test|spec)\.(?:js|jsx|mjs|cjs|ts|tsx)$", base) is not None
    is_test = is_test or base.endswith("Tests.swift")
    return "test" if is_test else "production"


def find_scc(root: Path) -> Path:
    requested = os.environ.get("CTX_LOC_SCC", "scc")
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
        raise GateError("pinned scc executable is unavailable; set CTX_LOC_SCC")
    return Path(located).resolve()


def read_policy(root: Path, has_git: bool) -> tuple[Path, dict[str, Any]]:
    configured = os.environ.get("CTX_LOC_POLICY_FILE", POLICY_PATH)
    policy_path = Path(configured)
    if not policy_path.is_absolute():
        policy_path = root / policy_path
    policy_path = policy_path.absolute()
    try:
        policy_path.relative_to(root)
    except ValueError as error:
        raise GateError("LOC policy must be inside the repository") from error
    if not policy_path.is_file():
        raise GateError(f"LOC policy does not exist: {configured}")
    if has_git and git(
        root,
        "ls-files",
        "--error-unmatch",
        str(policy_path.relative_to(root)),
        check=False,
    ).returncode != 0:
        raise GateError("LOC policy must be tracked in git")
    try:
        value = json.loads(policy_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise GateError(f"LOC policy is not valid UTF-8 JSON: {error}") from error
    if not isinstance(value, dict):
        raise GateError("LOC policy root must be an object")
    return policy_path, value


def validate_policy(policy: dict[str, Any]) -> tuple[dict[str, dict[str, int]], str, list[dict[str, Any]]]:
    expected_keys = {"schema_version", "policy", "metric", "limits", "grandfathered_at", "grandfathered"}
    if set(policy) != expected_keys or policy.get("schema_version") != 2:
        raise GateError("LOC policy schema is unsupported")
    if not isinstance(policy.get("policy"), str) or not policy["policy"].strip():
        raise GateError("LOC policy rationale must be nonempty")

    metric = policy.get("metric")
    if not isinstance(metric, dict) or set(metric) != {
        "tool",
        "version",
        "report_field",
        "archive_sha256",
        "binary_sha256",
    }:
        raise GateError("LOC metric configuration is malformed")
    if metric["tool"] != "scc" or metric["report_field"] != "Code":
        raise GateError("LOC metric must use the scc Code field")
    if not isinstance(metric["version"], str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", metric["version"]):
        raise GateError("scc version pin is malformed")
    for field in ("archive_sha256", "binary_sha256"):
        if not isinstance(metric[field], str) or SHA256.fullmatch(metric[field]) is None:
            raise GateError(f"scc {field} pin is malformed")

    limits = policy.get("limits")
    if not isinstance(limits, dict) or set(limits) != {"production", "test"}:
        raise GateError("LOC limits must define production and test")
    normalized_limits: dict[str, dict[str, int]] = {}
    for kind in ("production", "test"):
        value = limits[kind]
        if not isinstance(value, dict) or set(value) != {"advisory", "hard"}:
            raise GateError(f"LOC {kind} limits are malformed")
        advisory = value["advisory"]
        hard = value["hard"]
        if not isinstance(advisory, int) or isinstance(advisory, bool) or advisory <= 0:
            raise GateError(f"LOC {kind} advisory must be a positive integer")
        if not isinstance(hard, int) or isinstance(hard, bool) or hard <= advisory:
            raise GateError(f"LOC {kind} hard limit must be greater than its advisory")
        normalized_limits[kind] = {"advisory": advisory, "hard": hard}

    snapshot = policy.get("grandfathered_at")
    if not isinstance(snapshot, str) or COMMIT.fullmatch(snapshot) is None:
        raise GateError("grandfathered_at must be a full lowercase commit SHA")
    entries = policy.get("grandfathered")
    if not isinstance(entries, list):
        raise GateError("grandfathered entries must be an array")

    paths: list[str] = []
    normalized_entries: list[dict[str, Any]] = []
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"path", "kind", "code_baseline"}:
            raise GateError("grandfathered entry is malformed")
        path = normalized_repo_path(entry["path"])
        kind = entry["kind"]
        baseline = entry["code_baseline"]
        if kind not in normalized_limits:
            raise GateError(f"grandfathered kind is invalid for {path}")
        if classify(path) != kind:
            raise GateError(f"grandfathered kind does not match path classification: {path}")
        if not isinstance(baseline, int) or isinstance(baseline, bool) or baseline <= normalized_limits[kind]["hard"]:
            raise GateError(f"grandfathered baseline must exceed the hard limit: {path}")
        paths.append(path)
        normalized_entries.append({"path": path, "kind": kind, "code_baseline": baseline})
    if paths != sorted(paths):
        raise GateError("grandfathered entries must be sorted by path")
    if len(paths) != len(set(paths)):
        raise GateError("grandfathered paths must be unique")
    return normalized_limits, snapshot, normalized_entries


def verify_scc(scc: Path, metric: dict[str, Any]) -> None:
    actual_hash = hashlib.sha256(scc.read_bytes()).hexdigest()
    if actual_hash != metric["binary_sha256"]:
        raise GateError(
            f"scc binary hash mismatch: expected {metric['binary_sha256']}, got {actual_hash}"
        )
    result = subprocess.run(
        [str(scc), "--version"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        text=True,
    )
    actual_version = (result.stdout or result.stderr).strip()
    expected_version = f"scc version {metric['version']}"
    if result.returncode != 0 or actual_version != expected_version:
        raise GateError(f"scc version mismatch: expected {expected_version}, got {actual_version!r}")


def assert_no_symlink(root: Path, path: str) -> None:
    current = root
    for part in PurePosixPath(path).parts:
        current = current / part
        if current.is_symlink():
            raise GateError(f"counted source path crosses a symlink: {path}")


def inventory(root: Path, has_git: bool) -> list[str]:
    if has_git:
        result = git(root, "ls-files", "-z", "--cached", "--others", "--exclude-standard")
        raw_paths = result.stdout.split(b"\x00")
    else:
        raw_paths = [
            path.relative_to(root).as_posix().encode("utf-8")
            for path in root.rglob("*")
            if path.is_file()
        ]
    paths: list[str] = []
    for raw in raw_paths:
        if not raw:
            continue
        try:
            path = raw.decode("utf-8")
        except UnicodeDecodeError as error:
            raise GateError("counted source paths must be UTF-8") from error
        normalized_repo_path(path)
        absolute = root / path
        if not absolute.is_file():
            continue
        if classify(path) is None:
            continue
        if has_git:
            assert_no_symlink(root, path)
        paths.append(path)
    return sorted(set(paths))


def run_scc(scc: Path, cwd: Path, paths: list[str]) -> dict[str, int]:
    if not paths:
        return {}
    command = [
        str(scc),
        "--ci",
        "--count-as",
        "bazel:Bazel",
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
    ]
    result = subprocess.run(
        command,
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise GateError(f"scc failed: {detail}")
    try:
        report = json.loads(result.stdout)
    except (UnicodeError, json.JSONDecodeError) as error:
        raise GateError(f"scc returned malformed JSON: {error}") from error
    if not isinstance(report, list):
        raise GateError("scc JSON report root must be an array")

    counts: dict[str, int] = {}
    for language in report:
        if not isinstance(language, dict) or not isinstance(language.get("Files"), list):
            raise GateError("scc JSON report omits by-file data")
        for item in language["Files"]:
            if not isinstance(item, dict) or not isinstance(item.get("Location"), str):
                raise GateError("scc JSON file record is malformed")
            location = Path(item["Location"])
            if location.is_absolute():
                try:
                    path = location.resolve().relative_to(cwd.resolve()).as_posix()
                except ValueError as error:
                    raise GateError(f"scc reported a path outside its input root: {location}") from error
            else:
                path = PurePosixPath(item["Location"]).as_posix()
                if path.startswith("./"):
                    path = path[2:]
            code = item.get("Code")
            if path in counts or not isinstance(code, int) or isinstance(code, bool) or code < 0:
                raise GateError(f"scc JSON file record is invalid: {path}")
            counts[path] = code
    missing = sorted(set(paths) - set(counts))
    unexpected = sorted(set(counts) - set(paths))
    if missing or unexpected:
        detail = []
        if missing:
            detail.append(f"missing {', '.join(missing)}")
        if unexpected:
            detail.append(f"unexpected {', '.join(unexpected)}")
        raise GateError(f"scc report does not match the Git inventory: {'; '.join(detail)}")
    return counts


def revision_counts(root: Path, scc: Path, revision: str, paths: list[str]) -> dict[str, int]:
    if not paths:
        return {}
    with tempfile.TemporaryDirectory(prefix="ctx-loc-") as temporary:
        temporary_root = Path(temporary)
        for path in paths:
            blob = git(root, "show", f"{revision}:{path}", check=False)
            if blob.returncode != 0:
                raise GateError(f"grandfathered path does not exist at {revision}: {path}")
            destination = temporary_root / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(blob.stdout)
        return run_scc(scc, temporary_root, paths)


def resolve_revision(root: Path, value: str, label: str) -> str:
    result = git(root, "rev-parse", "--verify", f"{value}^{{commit}}", check=False)
    if result.returncode != 0:
        raise GateError(f"{label} is not a commit: {value}")
    return result.stdout.decode("utf-8").strip()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args()

    root, has_git = repo_context()
    _, policy = read_policy(root, has_git)
    limits, snapshot, entries = validate_policy(policy)
    scc = find_scc(root)
    verify_scc(scc, policy["metric"])

    entry_paths = [entry["path"] for entry in entries]
    if has_git:
        snapshot = resolve_revision(root, snapshot, "grandfathered snapshot")
        snapshot_counts = revision_counts(root, scc, snapshot, entry_paths)
        for entry in entries:
            path = entry["path"]
            if snapshot_counts[path] < entry["code_baseline"]:
                raise GateError(
                    f"grandfathered ceiling exceeds scc at {snapshot}: "
                    f"{path} was {snapshot_counts[path]}, policy says {entry['code_baseline']}"
                )

    paths = inventory(root, has_git)
    current_counts = run_scc(scc, root, paths)
    current_kinds = {path: classify(path) for path in paths}
    entry_by_path = {entry["path"]: entry for entry in entries}

    failures: list[tuple[int, str, str]] = []
    advisories: list[tuple[int, str, str]] = []
    for entry in entries:
        path = entry["path"]
        kind = entry["kind"]
        hard = limits[kind]["hard"]
        if has_git and git(root, "ls-files", "--error-unmatch", path, check=False).returncode != 0:
            failures.append((0, path, "stale grandfathered entry: path is not tracked"))
            continue
        if path not in current_counts:
            failures.append((0, path, "stale grandfathered entry: path is missing or no longer counted"))
            continue
        if current_kinds[path] != kind:
            failures.append((0, path, f"grandfathered kind is {kind}, current kind is {current_kinds[path]}"))
            continue
        current = current_counts[path]
        if current <= hard:
            failures.append(
                (0, path, f"stale grandfathered entry: {current} CLOC <= hard limit {hard}; remove it")
            )
            continue
        ceiling = entry["code_baseline"]
        if current > ceiling:
            failures.append(
                (
                    current - ceiling,
                    path,
                    f"{current} CLOC > shrink-ratchet ceiling {ceiling} (+{current - ceiling})",
                )
            )
            continue
        if current < ceiling:
            failures.append(
                (
                    0,
                    path,
                    f"stale shrink-ratchet ceiling: current is {current} CLOC "
                    f"< checked-in {ceiling}; lower code_baseline",
                )
            )
            continue
        advisories.append(
            (
                current,
                path,
                f"{current} CLOC > hard limit {hard}; grandfathered ceiling {ceiling}",
            )
        )

    for path in paths:
        if path in entry_by_path:
            continue
        kind = current_kinds[path]
        if kind is None:
            continue
        code = current_counts[path]
        advisory = limits[kind]["advisory"]
        hard = limits[kind]["hard"]
        if code > hard:
            failures.append((code - hard, path, f"{code} CLOC > hard limit {hard} (+{code - hard}); not grandfathered"))
        elif code > advisory:
            advisories.append((code, path, f"{code} CLOC > advisory {advisory} (hard {hard})"))

    if failures:
        print(
            "LOC gate failed; scc Code/CLOC limits are "
            f"production advisory={limits['production']['advisory']} hard={limits['production']['hard']}, "
            f"test advisory={limits['test']['advisory']} hard={limits['test']['hard']}.",
            file=sys.stderr,
        )
        print("Largest excess first:", file=sys.stderr)
        for _, path, detail in sorted(failures, key=lambda item: (-item[0], item[1])):
            print(f"  {path}: {detail}", file=sys.stderr)
        return 1

    if advisories:
        print(f"LOC advisory report (scc {policy['metric']['version']} Code/CLOC):")
        for _, path, detail in sorted(advisories, key=lambda item: (-item[0], item[1])):
            print(f"  {path}: {detail}")
    print(
        "LOC gate passed "
        f"({len(advisories)} advisories, {len(entries)} grandfathered hard-limit baselines; "
        "checked-in ceilings equal current CLOC; declared source inventory scanned)."
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as error:
        print(f"loc gate failed: {error}", file=sys.stderr)
        raise SystemExit(1) from None
