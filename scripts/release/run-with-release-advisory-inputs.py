#!/usr/bin/env python3
"""Acquire checked advisory inputs, then run one public release command."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request


ROOT = Path(__file__).resolve().parents[2]
SCANNER_ASSETS = {
    "linux-x64": "osv-scanner_linux_amd64",
    "linux-arm64": "osv-scanner_linux_arm64",
    "macos-arm64": "osv-scanner_darwin_arm64",
    "macos-x64": "osv-scanner_darwin_amd64",
    "windows-x64": "osv-scanner_windows_amd64.exe",
}
DATABASE_ECOSYSTEMS = ("crates.io", "npm")
HEX_64 = re.compile(r"[0-9a-f]{64}")
VERSION = re.compile(r"[0-9]+[.][0-9]+[.][0-9]+")


class InputError(Exception):
    """The release advisory inputs could not be acquired safely."""


def load_scanner_spec(policy_path: Path, target_id: str) -> tuple[str, str, str]:
    try:
        policy = json.loads(policy_path.read_bytes())
        scanner = policy["scanner"]
        version = scanner["version"]
        asset = SCANNER_ASSETS[target_id]
    except (KeyError, OSError, TypeError, json.JSONDecodeError) as error:
        raise InputError("release advisory scanner policy is incomplete") from error
    hashes = scanner.get("sha256_by_target")
    if hashes is None:
        if scanner.get("platform") != target_id:
            raise InputError("release advisory scanner policy does not cover target")
        expected_sha256 = scanner.get("sha256")
    else:
        if not isinstance(hashes, dict):
            raise InputError("release advisory scanner policy is invalid")
        expected_sha256 = hashes.get(target_id)
    if (
        policy.get("schema_version") != 1
        or scanner.get("name") != "osv-scanner"
        or not isinstance(version, str)
        or VERSION.fullmatch(version) is None
        or not isinstance(expected_sha256, str)
        or HEX_64.fullmatch(expected_sha256) is None
    ):
        raise InputError("release advisory scanner policy is invalid")
    return version, expected_sha256, asset


def download_scanner(
    destination: Path,
    version: str,
    expected_sha256: str,
    asset: str,
) -> None:
    url = (
        "https://github.com/google/osv-scanner/releases/download/"
        f"v{version}/{asset}"
    )
    request = urllib.request.Request(
        url,
        headers={"User-Agent": "ctx-release-advisory-inputs/1"},
    )
    destination.parent.mkdir(mode=0o700, parents=True)
    digest = hashlib.sha256()
    temporary: Path | None = None
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            with tempfile.NamedTemporaryFile(
                dir=destination.parent,
                delete=False,
            ) as output:
                temporary = Path(output.name)
                while block := response.read(1024 * 1024):
                    output.write(block)
                    digest.update(block)
        if digest.hexdigest() != expected_sha256:
            raise InputError("downloaded OSV-Scanner digest does not match policy")
        os.chmod(temporary, stat.S_IRUSR | stat.S_IXUSR)
        os.replace(temporary, destination)
        temporary = None
    except InputError:
        raise
    except (OSError, urllib.error.URLError) as error:
        raise InputError("could not download the pinned OSV-Scanner") from error
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def acquisition_environment() -> dict[str, str]:
    allowed = {
        "HOME",
        "PATH",
        "TMPDIR",
        "TEMP",
        "TMP",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "NO_PROXY",
        "https_proxy",
        "http_proxy",
        "no_proxy",
    }
    return {name: value for name, value in os.environ.items() if name in allowed}


def prepare_inputs(
    repo_root: Path,
    task_root: Path,
    target_id: str,
    ecosystems: tuple[str, ...] = ("crates.io",),
) -> tuple[Path, Path, Path, str]:
    version, expected_sha256, asset = load_scanner_spec(
        repo_root / "security/release-advisory-policy-v1.json",
        target_id,
    )
    scanner_name = "osv-scanner.exe" if asset.endswith(".exe") else "osv-scanner"
    scanner = task_root / "scanner" / scanner_name
    download_scanner(scanner, version, expected_sha256, asset)

    database = task_root / "database"
    metadata = task_root / "database-metadata.json"
    selected_ecosystems = tuple(sorted(set(ecosystems)))
    if not selected_ecosystems or any(
        ecosystem not in DATABASE_ECOSYSTEMS for ecosystem in selected_ecosystems
    ):
        raise InputError("release advisory database ecosystem is invalid")
    update_command = [
        sys.executable,
        "-I",
        str(repo_root / "scripts/update-release-advisory-db.py"),
        "--database-root",
        str(database),
        "--metadata",
        str(metadata),
    ]
    for ecosystem in selected_ecosystems:
        update_command.extend(["--ecosystem", ecosystem])
    update = subprocess.run(
        update_command,
        capture_output=True,
        check=False,
        env=acquisition_environment(),
        text=True,
    )
    if update.returncode != 0:
        detail = update.stderr.strip().splitlines()
        suffix = f": {detail[-1]}" if detail else ""
        raise InputError(f"could not refresh the OSV advisory database{suffix}")
    if (
        not scanner.is_file()
        or scanner.is_symlink()
        or not os.access(scanner, os.X_OK)
        or not database.is_dir()
        or database.is_symlink()
        or not metadata.is_file()
        or metadata.is_symlink()
    ):
        raise InputError("release advisory input preparation is incomplete")
    return scanner.resolve(), database.resolve(), metadata.resolve(), expected_sha256


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser()
    result.add_argument("--target", choices=sorted(SCANNER_ASSETS), required=True)
    result.add_argument(
        "--ecosystem",
        action="append",
        choices=DATABASE_ECOSYSTEMS,
        help="OSV ecosystem snapshot to provision (default: crates.io)",
    )
    result.add_argument("command", nargs=argparse.REMAINDER)
    return result


def main() -> int:
    os.umask(0o077)
    args = parser().parse_args()
    command = list(args.command)
    if command[:1] == ["--"]:
        command = command[1:]
    if not command:
        raise InputError("a release command is required after --")

    with tempfile.TemporaryDirectory(
        prefix=f"ctx-release-advisory-{args.target}.",
    ) as directory:
        task_root = Path(directory).resolve()
        scanner, database, metadata, scanner_sha256 = prepare_inputs(
            ROOT,
            task_root,
            args.target,
            tuple(args.ecosystem or ("crates.io",)),
        )
        print(
            "release advisory inputs prepared: "
            f"target={args.target} scanner_sha256={scanner_sha256}",
            flush=True,
        )
        command_environment = os.environ.copy()
        command_environment.update(
            {
                "CTX_OSV_SCANNER": str(scanner),
                "CTX_OSV_DATABASE_DIR": str(database),
                "CTX_OSV_DATABASE_METADATA": str(metadata),
            }
        )
        completed = subprocess.run(command, check=False, env=command_environment)
        if completed.returncode < 0:
            return 128 - completed.returncode
        return completed.returncode


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except InputError as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
