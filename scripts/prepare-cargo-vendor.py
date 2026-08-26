#!/usr/bin/env python3
"""Materialize Cargo's Bazel-declared sources as an offline vendor directory."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import sys
from urllib.parse import parse_qs, urlsplit, urlunsplit

try:
    import tomllib
except ModuleNotFoundError:  # Python 3.10 release workers use the Bazel tomli dep.
    import tomli as tomllib


def fail(message: str) -> None:
    raise SystemExit(f"prepare Cargo vendor failed: {message}")


def load_toml(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def quote(value: str) -> str:
    return json.dumps(value)


def copy_source(source: Path, destination: Path) -> None:
    def ignore(_directory: str, names: list[str]) -> set[str]:
        return {".cargo-checksum.json"} if ".cargo-checksum.json" in names else set()

    shutil.copytree(
        source,
        destination,
        copy_function=os.symlink,
        ignore=ignore,
    )


def package_key(
    metadata: dict,
    manifest: Path,
    locked: dict[tuple[str, str], list[dict]],
) -> tuple[str, str]:
    name = metadata.get("name")
    if not isinstance(name, str) or not name:
        fail(f"vendor manifest lacks a package name: {manifest}")
    version = metadata.get("version")
    if isinstance(version, str) and version:
        return name, version
    candidates = [key for key in locked if key[0] == name]
    if len(candidates) != 1:
        fail(
            f"cannot resolve workspace version for {name} from Cargo.lock; "
            f"found {len(candidates)} candidates"
        )
    return candidates[0]


def git_source_config(source: str) -> tuple[str, list[str]]:
    without_precise = source.split("#", 1)[0]
    raw_url = without_precise.removeprefix("git+")
    parsed = urlsplit(raw_url)
    query = parse_qs(parsed.query)
    git_url = urlunsplit((parsed.scheme, parsed.netloc, parsed.path, "", ""))
    lines = [f"[source.{quote(without_precise)}]", f"git = {quote(git_url)}"]
    for key in ("branch", "tag", "rev"):
        values = query.get(key, [])
        if values:
            if len(values) != 1:
                fail(f"git source has multiple {key} values: {source}")
            lines.append(f"{key} = {quote(values[0])}")
    lines.append('replace-with = "vendored-sources"')
    return without_precise, lines


def main() -> None:
    if len(sys.argv) != 4:
        fail("usage: prepare-cargo-vendor LOCKFILE VENDOR_MANIFEST CARGO_HOME")

    lockfile, vendor_manifest, cargo_home = map(Path, sys.argv[1:])
    test_srcdir = os.environ.get("TEST_SRCDIR")
    if not test_srcdir:
        fail("TEST_SRCDIR is required for declared crate source runfiles")
    if cargo_home.exists():
        fail(f"Cargo home already exists: {cargo_home}")

    packages = load_toml(lockfile).get("package", [])
    locked: dict[tuple[str, str], list[dict]] = {}
    for package in packages:
        source = package.get("source")
        if source:
            locked.setdefault((package["name"], package["version"]), []).append(package)

    runfiles_root = Path(test_srcdir)
    sources: dict[tuple[str, str], Path] = {}
    for relative_manifest in vendor_manifest.read_text(encoding="utf-8").splitlines():
        manifest = runfiles_root / relative_manifest
        metadata = load_toml(manifest).get("package", {})
        key = package_key(metadata, manifest, locked)
        if key in sources and sources[key] != manifest.parent:
            fail(f"duplicate declared source for {key[0]} {key[1]}")
        sources[key] = manifest.parent

    cargo_home.mkdir(parents=True)
    vendor_dir = cargo_home / "vendor"
    vendor_dir.mkdir()
    git_configs: dict[str, list[str]] = {}
    for key, source_root in sorted(sources.items()):
        matches = locked.get(key, [])
        if len(matches) != 1:
            fail(f"expected one locked source for {key[0]} {key[1]}, found {len(matches)}")
        package = matches[0]
        source = package["source"]
        destination = vendor_dir / f"{key[0]}-{key[1]}"
        copy_source(source_root, destination)
        (destination / ".cargo-checksum.json").write_text(
            json.dumps(
                {"files": {}, "package": package.get("checksum")},
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
        )
        if source.startswith("git+"):
            name, lines = git_source_config(source)
            git_configs[name] = lines
        elif not source.startswith(("registry+", "sparse+")):
            fail(f"unsupported Cargo source: {source}")

    config = [
        "[source.crates-io]",
        'replace-with = "vendored-sources"',
        "",
    ]
    for lines in git_configs.values():
        config.extend(lines)
        config.append("")
    config.extend(
        [
            "[source.vendored-sources]",
            f"directory = {quote(str(vendor_dir))}",
            "",
            "[net]",
            "offline = true",
            "",
        ]
    )
    (cargo_home / "config.toml").write_text("\n".join(config), encoding="utf-8")


if __name__ == "__main__":
    main()
