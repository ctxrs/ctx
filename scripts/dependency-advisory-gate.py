#!/usr/bin/env python3
"""Fail-closed advisory gate for declared release dependency lockfiles."""

from __future__ import annotations

import argparse
from datetime import UTC, date, datetime, timedelta
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tempfile
import tomllib
from typing import Any


LOCKFILE_NAMES = {
    "Cargo.lock",
    "npm-shrinkwrap.json",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
}
SKIP_DIRECTORIES = {
    ".artifacts",
    ".cache",
    ".git",
    "bazel-bin",
    "bazel-out",
    "bazel-testlogs",
    "node_modules",
    "target",
}
HEX_64 = re.compile(r"[0-9a-f]{64}")
ADVISORY_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:+-]{2,127}")
STATUS_EXIT = {
    "clean": 0,
    "advisory": 10,
    "expired_exception": 11,
    "unknown_exception": 12,
    "stale_database": 20,
    "tool_failure": 21,
}


class GateError(Exception):
    def __init__(self, status: str, message: str):
        super().__init__(message)
        self.status = status


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def read_json(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError) as error:
        raise GateError("tool_failure", f"{label} is unavailable or malformed") from error
    if not isinstance(value, dict):
        raise GateError("tool_failure", f"{label} must be a JSON object")
    return value


def parse_time(value: Any, label: str) -> datetime:
    if not isinstance(value, str) or not value.endswith("Z"):
        raise GateError("tool_failure", f"{label} must be a UTC timestamp")
    try:
        result = datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as error:
        raise GateError("tool_failure", f"{label} is invalid") from error
    return result.astimezone(UTC)


def safe_relative(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise GateError("tool_failure", f"{label} must be a non-empty path")
    path = PurePosixPath(value)
    if path.is_absolute() or ".." in path.parts or str(path) != value:
        raise GateError("tool_failure", f"{label} is not a safe repository path")
    return value


def tracked_lockfiles(repo_root: Path) -> set[str]:
    try:
        top_level = subprocess.run(
            ["git", "-C", str(repo_root), "rev-parse", "--show-toplevel"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        if Path(top_level).resolve() != repo_root.resolve():
            raise subprocess.CalledProcessError(1, "git")
        result = subprocess.run(
            ["git", "-C", str(repo_root), "ls-files", "-z"],
            check=True,
            capture_output=True,
        )
    except (OSError, subprocess.CalledProcessError):
        result = None
    if result is not None:
        return {
            value
            for raw in result.stdout.split(b"\0")
            if raw
            for value in [raw.decode("utf-8")]
            if PurePosixPath(value).name in LOCKFILE_NAMES
        }

    found: set[str] = set()
    for directory, names, files in os.walk(repo_root):
        names[:] = [
            name
            for name in names
            if name not in SKIP_DIRECTORIES and not name.startswith("bazel-")
        ]
        base = Path(directory)
        for name in files:
            if name in LOCKFILE_NAMES:
                found.add((base / name).relative_to(repo_root).as_posix())
    return found


def validate_policy(
    policy: dict[str, Any], repo_root: Path
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    if policy.get("schema_version") != 1:
        raise GateError("tool_failure", "advisory policy schema is unsupported")
    scanner = policy.get("scanner")
    lockfiles = policy.get("lockfiles")
    if not isinstance(scanner, dict) or not isinstance(lockfiles, list) or not lockfiles:
        raise GateError("tool_failure", "advisory policy is incomplete")
    if (
        scanner.get("name") != "osv-scanner"
        or not isinstance(scanner.get("version"), str)
        or HEX_64.fullmatch(scanner.get("sha256", "")) is None
        or not isinstance(scanner.get("max_database_age_hours"), int)
        or scanner["max_database_age_hours"] < 1
    ):
        raise GateError("tool_failure", "advisory scanner policy is invalid")

    declared: dict[str, dict[str, Any]] = {}
    for index, entry in enumerate(lockfiles):
        if not isinstance(entry, dict):
            raise GateError("tool_failure", f"lockfile policy entry {index} is invalid")
        path = safe_relative(entry.get("path"), f"lockfile policy entry {index}")
        if path in declared:
            raise GateError("tool_failure", f"duplicate lockfile policy path: {path}")
        if entry.get("ecosystem") not in {"crates.io", "npm"}:
            raise GateError("tool_failure", f"unsupported lockfile ecosystem: {path}")
        if entry.get("disposition") not in {"scan", "exclude"}:
            raise GateError("tool_failure", f"invalid lockfile disposition: {path}")
        if not isinstance(entry.get("role"), str) or not entry["role"].strip():
            raise GateError("tool_failure", f"lockfile role is missing: {path}")
        if entry["disposition"] == "exclude":
            if not isinstance(entry.get("rationale"), str) or len(entry["rationale"]) < 20:
                raise GateError("tool_failure", f"lockfile exclusion lacks rationale: {path}")
        elif entry.get("closure") not in {
            "bazel-release-inventory",
            "cargo-release-union",
            "lockfile",
        }:
            raise GateError("tool_failure", f"lockfile closure is invalid: {path}")
        declared[path] = entry

    observed = tracked_lockfiles(repo_root)
    unknown = sorted(observed - declared.keys())
    missing = sorted(declared.keys() - observed)
    if unknown:
        raise GateError(
            "tool_failure", "unreviewed dependency lockfiles: " + ", ".join(unknown)
        )
    if missing:
        raise GateError(
            "tool_failure", "declared dependency lockfiles are missing: " + ", ".join(missing)
        )
    return [entry for entry in lockfiles if entry["disposition"] == "scan"], scanner


def validate_database(
    metadata_path: Path,
    database_root: Path,
    ecosystems: set[str],
    scanner_policy: dict[str, Any],
    now: datetime,
) -> dict[str, Any]:
    metadata = read_json(metadata_path, "OSV database metadata")
    records = metadata.get("databases")
    if metadata.get("schema_version") != 1 or not isinstance(records, list):
        raise GateError("tool_failure", "OSV database metadata schema is unsupported")
    by_ecosystem = {
        record.get("ecosystem"): record
        for record in records
        if isinstance(record, dict) and isinstance(record.get("ecosystem"), str)
    }
    validated = []
    oldest = now
    for ecosystem in sorted(ecosystems):
        record = by_ecosystem.get(ecosystem)
        if record is None:
            raise GateError("tool_failure", f"OSV database is unavailable: {ecosystem}")
        relative = safe_relative(record.get("path"), f"{ecosystem} database path")
        expected_hash = record.get("sha256")
        expected_size = record.get("size")
        if HEX_64.fullmatch(expected_hash or "") is None or not isinstance(
            expected_size, int
        ):
            raise GateError("tool_failure", f"OSV database metadata is invalid: {ecosystem}")
        path = database_root / relative
        try:
            actual_size = path.stat().st_size
        except OSError as error:
            raise GateError(
                "tool_failure", f"OSV database is unavailable: {ecosystem}"
            ) from error
        if actual_size != expected_size or sha256_file(path) != expected_hash:
            raise GateError("tool_failure", f"OSV database digest mismatch: {ecosystem}")
        modified = parse_time(
            record.get("source_last_modified"),
            f"{ecosystem} database source_last_modified",
        )
        if modified > now + timedelta(minutes=5):
            raise GateError("tool_failure", f"OSV database timestamp is in the future: {ecosystem}")
        oldest = min(oldest, modified)
        validated.append(
            {
                "ecosystem": ecosystem,
                "sha256": expected_hash,
                "size": expected_size,
                "source_generation": record.get("source_generation"),
                "source_last_modified": modified.isoformat().replace("+00:00", "Z"),
            }
        )
    max_age = timedelta(hours=scanner_policy["max_database_age_hours"])
    if now - oldest > max_age:
        raise GateError("stale_database", "OSV advisory database is stale")
    return {
        "metadata_sha256": sha256_file(metadata_path),
        "oldest_source_timestamp": oldest.isoformat().replace("+00:00", "Z"),
        "records": validated,
    }


def scanner_version(
    scanner: Path, expected_version: str, expected_sha256: str
) -> dict[str, Any]:
    try:
        actual_sha256 = sha256_file(scanner)
    except OSError as error:
        raise GateError("tool_failure", "OSV-Scanner is unavailable") from error
    if actual_sha256 != expected_sha256:
        raise GateError("tool_failure", "OSV-Scanner digest mismatch")
    try:
        result = subprocess.run(
            [str(scanner), "--version"], check=True, capture_output=True, text=True
        )
    except (OSError, subprocess.CalledProcessError) as error:
        raise GateError("tool_failure", "OSV-Scanner is unavailable") from error
    match = re.search(r"osv-scanner version:\s*([0-9][^\s]*)", result.stdout)
    if match is None:
        match = re.search(r"\bVERSION:\s*([0-9][^\s]*)", result.stdout)
    if match is None or match.group(1) != expected_version:
        observed = match.group(1) if match else "unknown"
        raise GateError(
            "tool_failure",
            "OSV-Scanner version mismatch: "
            f"expected {expected_version}, observed {observed}",
        )
    return {
        "name": "osv-scanner",
        "version": match.group(1),
        "sha256": actual_sha256,
    }


def package_key(package: dict[str, Any]) -> tuple[str, str, str]:
    values = (
        package.get("ecosystem"),
        package.get("name"),
        package.get("version"),
    )
    if not all(isinstance(value, str) and value for value in values):
        raise GateError("tool_failure", "OSV-Scanner emitted a malformed package")
    return values  # type: ignore[return-value]


def cargo_dependency_identity(
    value: str, packages: list[dict[str, Any]]
) -> tuple[str, str, str]:
    match = re.fullmatch(r"([^ ]+)(?: ([^ ]+))?(?: \((.+)\))?", value)
    if match is None:
        raise GateError("tool_failure", f"Cargo.lock dependency is malformed: {value}")
    name, version, source = match.groups()
    candidates = [
        (item["name"], item["version"], item.get("source", ""))
        for item in packages
        if item.get("name") == name
        and (version is None or item.get("version") == version)
        and (source is None or item.get("source", "") == source)
    ]
    if len(candidates) != 1:
        raise GateError("tool_failure", f"Cargo.lock dependency is ambiguous: {value}")
    return candidates[0]


def cargo_release_union(
    lock_path: Path, manifest_path: Path, scanner_packages: set[tuple[str, str, str]]
) -> set[tuple[str, str, str]]:
    try:
        lock = tomllib.loads(lock_path.read_text(encoding="utf-8"))
        manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise GateError("tool_failure", "Cargo release closure inputs are malformed") from error
    packages = lock.get("package")
    root_name = manifest.get("package", {}).get("name")
    if not isinstance(packages, list) or not isinstance(root_name, str):
        raise GateError("tool_failure", "Cargo release closure inputs are incomplete")
    roots = [
        item
        for item in packages
        if item.get("name") == root_name and not item.get("source")
    ]
    if len(roots) != 1:
        raise GateError("tool_failure", "Cargo release root package is ambiguous")

    direct_names = set()
    for section in ("dependencies", "build-dependencies"):
        direct_names.update(manifest.get(section, {}).keys())
    for target in manifest.get("target", {}).values():
        if isinstance(target, dict):
            for section in ("dependencies", "build-dependencies"):
                direct_names.update(target.get(section, {}).keys())
    root_dependencies = [
        cargo_dependency_identity(value, packages)
        for value in roots[0].get("dependencies", [])
        if value.split(" ", 1)[0] in direct_names
    ]
    by_identity = {
        (item["name"], item["version"], item.get("source", "")): item
        for item in packages
    }
    selected = set(root_dependencies)
    pending = list(root_dependencies)
    while pending:
        identity = pending.pop()
        for value in by_identity[identity].get("dependencies", []):
            dependency = cargo_dependency_identity(value, packages)
            if dependency not in selected:
                selected.add(dependency)
                pending.append(dependency)
    release = {
        ("crates.io", name, version)
        for name, version, source in selected
        if source.startswith("registry+")
    }
    missing = sorted(release - scanner_packages)
    if missing:
        raise GateError("tool_failure", "OSV-Scanner omitted Cargo release packages")
    return release


def bazel_release_inventory(
    inventory_path: Path, scanner_packages: set[tuple[str, str, str]]
) -> set[tuple[str, str, str]]:
    try:
        labels = inventory_path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError) as error:
        raise GateError("tool_failure", "Bazel dependency inventory is unavailable") from error
    if not labels or labels != sorted(set(labels)):
        raise GateError("tool_failure", "Bazel dependency inventory is malformed")
    selected: set[tuple[str, str, str]] = set()
    crate_labels = [label for label in labels if "crates__" in label]
    for label in crate_labels:
        # Avoid parsing crate names and semver delimiters: match the complete
        # repository token already emitted by rules_rust.
        candidates = [
            package
            for package in scanner_packages
            if package[0] == "crates.io"
            and f"crates__{package[1]}-{package[2].replace('+', '-')}//" in label
        ]
        if len(candidates) != 1:
            raise GateError("tool_failure", f"Bazel inventory label is ambiguous: {label}")
        selected.add(candidates[0])
    if not selected:
        raise GateError("tool_failure", "Bazel inventory selected no registry crates")
    return selected


def scan_with_osv(
    scanner: Path,
    database_root: Path,
    repo_root: Path,
    entries: list[dict[str, Any]],
) -> tuple[dict[str, Any], int, str]:
    command = [
        str(scanner),
        "scan",
        "source",
        "--offline",
        "--no-resolve",
        "--all-packages",
        "--format",
        "json",
        "--verbosity",
        "error",
    ]
    for entry in entries:
        command.extend(["-L", str(repo_root / entry["path"])])
    environment = os.environ.copy()
    environment["OSV_SCANNER_LOCAL_DB_CACHE_DIRECTORY"] = str(database_root)
    try:
        result = subprocess.run(
            command, cwd=repo_root, env=environment, capture_output=True, text=True
        )
    except OSError as error:
        raise GateError("tool_failure", "OSV-Scanner execution failed") from error
    if result.returncode not in {0, 1}:
        raise GateError(
            "tool_failure", f"OSV-Scanner failed with exit code {result.returncode}"
        )
    try:
        output = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise GateError("tool_failure", "OSV-Scanner JSON output is malformed") from error
    if not isinstance(output, dict) or not isinstance(output.get("results"), list):
        raise GateError("tool_failure", "OSV-Scanner JSON output is incomplete")
    return output, result.returncode, result.stderr


def parse_exceptions(value: dict[str, Any]) -> list[dict[str, Any]]:
    entries = value.get("exceptions")
    if value.get("schema_version") != 1 or not isinstance(entries, list):
        raise GateError("tool_failure", "advisory exception ledger is malformed")
    required = {
        "advisory_id",
        "ecosystem",
        "package",
        "version",
        "lockfile",
        "rationale",
        "owner",
        "expires",
    }
    seen = set()
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != required:
            raise GateError("tool_failure", "advisory exception entry is malformed")
        identity = tuple(entry[key] for key in sorted(required - {"rationale", "owner", "expires"}))
        if identity in seen:
            raise GateError("tool_failure", "duplicate advisory exception")
        seen.add(identity)
        if ADVISORY_ID.fullmatch(entry["advisory_id"]) is None:
            raise GateError("tool_failure", "advisory exception ID is invalid")
        safe_relative(entry["lockfile"], "advisory exception lockfile")
        if (
            not all(
                isinstance(entry[key], str) and entry[key].strip()
                for key in ("ecosystem", "package", "version", "owner")
            )
            or not isinstance(entry["rationale"], str)
            or len(entry["rationale"].strip()) < 20
        ):
            raise GateError("tool_failure", "advisory exception review fields are invalid")
        try:
            date.fromisoformat(entry["expires"])
        except (TypeError, ValueError) as error:
            raise GateError("tool_failure", "advisory exception expiry is invalid") from error
    return entries


def severity_for(item: dict[str, Any], advisory_id: str) -> str | None:
    for group in item.get("groups", []):
        if advisory_id in group.get("ids", []):
            value = group.get("max_severity")
            return value if isinstance(value, str) and value else None
    return None


def evaluate(
    args: argparse.Namespace, now: datetime, receipt: dict[str, Any]
) -> tuple[str, str | None]:
    repo_root = args.repo_root.resolve()
    try:
        source_commit = subprocess.run(
            ["git", "-C", str(repo_root), "rev-parse", "--verify", "HEAD^{commit}"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        source_dirty = bool(
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(repo_root),
                    "status",
                    "--porcelain=v1",
                    "--untracked-files=all",
                ],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
        )
        receipt["source"] = {"commit": source_commit, "dirty": source_dirty}
    except (OSError, subprocess.CalledProcessError):
        receipt["source"] = {"commit": None, "dirty": None}
    policy = read_json(args.policy, "advisory policy")
    entries, scanner_policy = validate_policy(policy, repo_root)
    ecosystems = {entry["ecosystem"] for entry in entries}
    receipt["policy"] = {
        "path": args.policy.name,
        "sha256": sha256_file(args.policy),
    }
    receipt["database"] = validate_database(
        args.database_metadata,
        args.database_root,
        ecosystems,
        scanner_policy,
        now,
    )
    receipt["scanner"] = scanner_version(
        args.scanner,
        scanner_policy["version"],
        scanner_policy["sha256"],
    )
    exceptions_value = read_json(args.exceptions, "advisory exception ledger")
    exceptions = parse_exceptions(exceptions_value)
    receipt["exception_ledger"] = {
        "sha256": sha256_file(args.exceptions),
        "entry_count": len(exceptions),
    }
    receipt["lockfiles"] = [
        {
            "path": entry["path"],
            "ecosystem": entry["ecosystem"],
            "closure": entry["closure"],
            "role": entry["role"],
            "sha256": sha256_file(repo_root / entry["path"]),
        }
        for entry in entries
    ]

    output, scanner_exit, scanner_stderr = scan_with_osv(
        args.scanner, args.database_root, repo_root, entries
    )
    receipt["scanner_result"] = {
        "exit_code": scanner_exit,
        "json_sha256": hashlib.sha256(
            json.dumps(output, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest(),
        "stderr_sha256": hashlib.sha256(scanner_stderr.encode()).hexdigest(),
    }

    expected_paths = {str((repo_root / entry["path"]).resolve()): entry for entry in entries}
    results: dict[str, dict[str, Any]] = {}
    for result in output["results"]:
        if not isinstance(result, dict) or not isinstance(result.get("source"), dict):
            raise GateError("tool_failure", "OSV-Scanner result source is malformed")
        source = result["source"].get("path")
        if not isinstance(source, str):
            raise GateError("tool_failure", "OSV-Scanner result path is malformed")
        source_path = str(Path(source).resolve())
        if source_path not in expected_paths or source_path in results:
            raise GateError("tool_failure", "OSV-Scanner returned an unexpected source")
        results[source_path] = result
    if results.keys() != expected_paths.keys():
        raise GateError("tool_failure", "OSV-Scanner omitted a declared lockfile")

    findings = []
    package_counts = {}
    for source_path, entry in expected_paths.items():
        result = results[source_path]
        packages = result.get("packages")
        if not isinstance(packages, list) or not packages:
            raise GateError("tool_failure", f"OSV-Scanner found no packages: {entry['path']}")
        scanner_packages = {package_key(item.get("package", {})) for item in packages}
        package_counts[entry["path"]] = len(scanner_packages)
        if entry["closure"] == "bazel-release-inventory":
            if args.cargo_inventory is None:
                raise GateError("tool_failure", "Bazel Cargo inventory was not supplied")
            selected = bazel_release_inventory(args.cargo_inventory, scanner_packages)
        elif entry["closure"] == "cargo-release-union":
            manifest = safe_relative(entry.get("manifest"), "Cargo release manifest")
            selected = cargo_release_union(
                repo_root / entry["path"], repo_root / manifest, scanner_packages
            )
        else:
            selected = scanner_packages

        for item in packages:
            package = package_key(item.get("package", {}))
            vulnerabilities = item.get("vulnerabilities", [])
            if not isinstance(vulnerabilities, list):
                raise GateError("tool_failure", "OSV-Scanner vulnerabilities are malformed")
            for vulnerability in vulnerabilities:
                advisory_id = vulnerability.get("id")
                if not isinstance(advisory_id, str) or ADVISORY_ID.fullmatch(advisory_id) is None:
                    raise GateError("tool_failure", "OSV-Scanner advisory ID is malformed")
                findings.append(
                    {
                        "advisory_id": advisory_id,
                        "ecosystem": package[0],
                        "package": package[1],
                        "version": package[2],
                        "lockfile": entry["path"],
                        "summary": vulnerability.get("summary"),
                        "severity": severity_for(item, advisory_id),
                        "classification": (
                            "unreviewed"
                            if package in selected
                            else "outside_release_closure"
                        ),
                    }
                )
    findings.sort(
        key=lambda item: (
            item["lockfile"],
            item["ecosystem"],
            item["package"],
            item["version"],
            item["advisory_id"],
        )
    )
    in_scope = [item for item in findings if item["classification"] == "unreviewed"]
    if (scanner_exit == 0) != (not findings):
        raise GateError("tool_failure", "OSV-Scanner exit status contradicts its findings")

    matched_exception_indexes = set()
    expired = []
    for finding in in_scope:
        matches = [
            (index, exception)
            for index, exception in enumerate(exceptions)
            if all(
                exception[field] == finding[field]
                for field in (
                    "advisory_id",
                    "ecosystem",
                    "package",
                    "version",
                    "lockfile",
                )
            )
        ]
        if len(matches) > 1:
            raise GateError("tool_failure", "multiple exceptions match one advisory")
        if matches:
            index, exception = matches[0]
            matched_exception_indexes.add(index)
            if now.date() > date.fromisoformat(exception["expires"]):
                finding["classification"] = "expired_exception"
                expired.append(finding)
            else:
                finding["classification"] = "reviewed_exception"
                finding["exception"] = {
                    "expires": exception["expires"],
                    "owner": exception["owner"],
                    "rationale": exception["rationale"],
                }
    unknown = [
        exceptions[index]
        for index in range(len(exceptions))
        if index not in matched_exception_indexes
    ]
    unreviewed = [
        item for item in in_scope if item["classification"] == "unreviewed"
    ]
    receipt["coverage"] = {
        "dependency_lockfiles": sorted(expected_paths.values(), key=lambda item: item["path"]),
        "os_packages_scanned": False,
        "container_images_scanned": False,
        "package_counts": package_counts,
    }
    receipt["findings"] = findings
    receipt["summary"] = {
        "expired_exception_count": len(expired),
        "outside_release_closure_count": sum(
            item["classification"] == "outside_release_closure" for item in findings
        ),
        "reviewed_exception_count": sum(
            item["classification"] == "reviewed_exception" for item in findings
        ),
        "unknown_exception_count": len(unknown),
        "unreviewed_advisory_count": len(unreviewed),
    }
    if unknown:
        receipt["unknown_exceptions"] = unknown
        return "unknown_exception", "exception ledger contains unmatched entries"
    if expired:
        return "expired_exception", "one or more advisory exceptions expired"
    if unreviewed:
        return "advisory", "release closure contains unreviewed advisories"
    return "clean", None


def write_receipt(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(value, indent=2, sort_keys=True) + "\n"
    with tempfile.NamedTemporaryFile(
        mode="w", encoding="utf-8", dir=path.parent, delete=False
    ) as temporary:
        temporary.write(payload)
        temporary_path = Path(temporary.name)
    os.replace(temporary_path, path)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", type=Path, required=True)
    parser.add_argument("--policy", type=Path, required=True)
    parser.add_argument("--exceptions", type=Path, required=True)
    parser.add_argument("--database-root", type=Path, required=True)
    parser.add_argument("--database-metadata", type=Path, required=True)
    parser.add_argument("--scanner", type=Path, required=True)
    parser.add_argument("--cargo-inventory", type=Path)
    parser.add_argument("--target-id", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--now", help=argparse.SUPPRESS)
    args = parser.parse_args()
    now = parse_time(args.now, "--now") if args.now else datetime.now(UTC)
    receipt: dict[str, Any] = {
        "schema_version": 1,
        "generated_at": now.isoformat().replace("+00:00", "Z"),
        "target_id": args.target_id,
    }
    try:
        status, reason = evaluate(args, now, receipt)
    except GateError as error:
        status, reason = error.status, str(error)
    except Exception as error:
        status, reason = "tool_failure", f"unexpected gate failure: {type(error).__name__}"
    receipt["status"] = status
    if reason:
        receipt["failure_reason"] = reason
    write_receipt(args.output, receipt)
    print(f"dependency advisory gate: {status}: {args.output}", file=sys.stderr)
    return STATUS_EXIT[status]


if __name__ == "__main__":
    raise SystemExit(main())
