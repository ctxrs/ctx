#!/usr/bin/env python3
"""Prepare an offline Cargo workspace from Bazel-declared SDK inputs."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import shutil
import sys
import tomllib


SDK_DEPENDENCIES = ("serde", "serde_json", "tempfile", "thiserror")
WORKSPACE_PACKAGE_KEYS = (
    "edition",
    "license",
    "repository",
    "homepage",
    "rust-version",
)


def fail(message: str) -> None:
    raise SystemExit(f"prepare SDK Cargo workspace failed: {message}")


def quote(value: str) -> str:
    return json.dumps(value)


def load_toml(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def dependency_line(name: str, spec: object, source: Path, version: str) -> str:
    fields = [f"version = {quote('=' + version)}", f"path = {quote(str(source))}"]
    if isinstance(spec, dict):
        features = spec.get("features")
        if features:
            fields.append("features = [{}]".format(", ".join(quote(item) for item in features)))
        if spec.get("default-features") is False:
            fields.append("default-features = false")
    return f"{name} = {{ {', '.join(fields)} }}"


def remove_unavailable_optional_dependencies(manifest: Path, available: set[str]) -> None:
    lines = manifest.read_text(encoding="utf-8").splitlines(keepends=True)
    sections: list[list[str]] = []
    current: list[str] = []
    for line in lines:
        if line.startswith("[") and line.rstrip().endswith("]"):
            if current:
                sections.append(current)
            current = [line]
        else:
            current.append(line)
    if current:
        sections.append(current)

    retained_sections: list[list[str]] = []
    dropped: set[str] = set()
    dependency_section = re.compile(r"^\[(?:dependencies|target\..*\.dependencies)\.([^]]+)\]$")
    dev_dependency_section = re.compile(
        r"^\[(?:dev-dependencies|target\..*\.dev-dependencies)\.([^]]+)\]$"
    )
    for section in sections:
        heading = section[0].strip() if section else ""
        if heading.startswith("[dev-dependencies") or re.match(
            r"^\[target\..*\.dev-dependencies", heading
        ):
            match = dev_dependency_section.match(heading)
            if match:
                dropped.add(match.group(1).strip('"'))
            continue
        match = dependency_section.match(heading)
        dependency = match.group(1).strip('"') if match else None
        optional = any(line.strip() == "optional = true" for line in section[1:])
        target_specific = heading.startswith("[target.")
        if dependency and dependency not in available and (optional or target_specific):
            dropped.add(dependency)
            continue
        retained_sections.append(section)

    retained: list[str] = []
    for section in retained_sections:
        if section and section[0].strip() == "[features]" and dropped:
            retained.append(section[0])
            for line in section[1:]:
                references_dropped = any(
                    re.search(rf'"(?:dep:)?{re.escape(dependency)}(?:[/?][^"]*)?"', line)
                    for dependency in dropped
                )
                if not references_dropped:
                    retained.append(line)
        else:
            retained.extend(section)
    manifest.write_text("".join(retained), encoding="utf-8")


def main() -> None:
    if len(sys.argv) != 7:
        fail(
            "usage: prepare-sdk-cargo-workspace.py "
            "ROOT_CARGO ROOT_LOCK PROTOCOL_DIR SDK_DIR VENDOR_MANIFEST DEST"
        )

    root_cargo, root_lock, protocol_dir, sdk_dir, vendor_manifest, destination = map(
        Path, sys.argv[1:]
    )
    if destination.exists():
        fail(f"destination already exists: {destination}")

    root_metadata = load_toml(root_cargo)
    lock_metadata = load_toml(root_lock)
    workspace_dependencies = root_metadata["workspace"]["dependencies"]
    lock_packages = lock_metadata["package"]
    test_srcdir = os.environ.get("TEST_SRCDIR")
    if not test_srcdir:
        fail("TEST_SRCDIR is required for declared crate source runfiles")
    runfiles_root = Path(test_srcdir)
    source_roots: dict[tuple[str, str], Path] = {}
    for relative_manifest in vendor_manifest.read_text(encoding="utf-8").splitlines():
        manifest = runfiles_root / relative_manifest
        metadata = load_toml(manifest)["package"]
        source_roots[(metadata["name"], metadata["version"])] = manifest.parent

    destination.mkdir(parents=True)
    vendor_dir = destination / "vendor"
    vendor_dir.mkdir()
    vendored_by_name: dict[str, tuple[Path, str]] = {}
    for (name, version), source_root in sorted(source_roots.items()):
        vendored = vendor_dir / f"{name}-{version}"
        shutil.copytree(source_root, vendored)
        package = next(
            (
                item
                for item in lock_packages
                if item["name"] == name and item["version"] == version
            ),
            None,
        )
        checksum = package.get("checksum") if package else None
        (vendored / ".cargo-checksum.json").write_text(
            json.dumps({"files": {}, "package": checksum}, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        if name in SDK_DEPENDENCIES:
            vendored_by_name[name] = (vendored, version)

    available_names = {name for name, _version in source_roots}
    for vendored in vendor_dir.iterdir():
        remove_unavailable_optional_dependencies(vendored / "Cargo.toml", available_names)

    missing = sorted(set(SDK_DEPENDENCIES) - set(vendored_by_name))
    if missing:
        fail("missing declared crate sources: " + ", ".join(missing))

    crates_dir = destination / "crates"
    shutil.copytree(protocol_dir, crates_dir / "ctx-protocol")
    shutil.copytree(sdk_dir, crates_dir / "ctx-sdk")

    workspace_package = root_metadata["workspace"]["package"]
    lines = [
        "[workspace]",
        'members = ["crates/ctx-protocol", "crates/ctx-sdk"]',
        'resolver = "2"',
        "",
        "[workspace.package]",
    ]
    lines.extend(
        f"{key} = {quote(str(workspace_package[key]))}" for key in WORKSPACE_PACKAGE_KEYS
    )
    lines.extend(["", "[workspace.dependencies]"])
    for name in SDK_DEPENDENCIES:
        vendored, version = vendored_by_name[name]
        lines.append(
            dependency_line(
                name,
                workspace_dependencies[name],
                vendored,
                version,
            )
        )
    (destination / "Cargo.toml").write_text("\n".join(lines) + "\n", encoding="utf-8")

    cargo_config = destination / ".cargo" / "config.toml"
    cargo_config.parent.mkdir()
    cargo_config.write_text(
        "[source.crates-io]\n"
        'replace-with = "vendored-sources"\n\n'
        "[source.vendored-sources]\n"
        f"directory = {quote(str(vendor_dir))}\n\n"
        "[net]\n"
        "offline = true\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    main()
