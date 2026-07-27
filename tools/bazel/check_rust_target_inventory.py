#!/usr/bin/env python3
"""Fail closed when a Cargo target lacks explicit native Bazel ownership."""

from __future__ import annotations

import json
import pathlib
import re
import sys
import tomllib


def fail(message: str) -> None:
    raise SystemExit(f"rust target inventory check failed: {message}")


def discover(manifest_path: pathlib.Path) -> set[str]:
    data = tomllib.loads(manifest_path.read_text())
    package = data["package"]
    crate_dir = manifest_path.parent
    found: set[str] = set()
    lib = data.get("lib")
    if lib or (crate_dir / "src/lib.rs").is_file():
        name = (lib or {}).get("name", package["name"].replace("-", "_"))
        found.add(f"lib:{name}")
    explicit_bins = data.get("bin", [])
    if explicit_bins:
        found.update(f"bin:{item['name']}" for item in explicit_bins)
    else:
        if (crate_dir / "src/main.rs").is_file():
            found.add(f"bin:{package['name']}")
        found.update(f"bin:{path.stem}" for path in (crate_dir / "src/bin").glob("*.rs"))
    explicit_tests = data.get("test", [])
    if explicit_tests:
        found.update(f"test:{item['name']}" for item in explicit_tests)
    else:
        found.update(f"test:{path.stem}" for path in (crate_dir / "tests").glob("*.rs"))
    explicit_examples = data.get("example", [])
    if explicit_examples:
        found.update(f"example:{item['name']}" for item in explicit_examples)
    else:
        found.update(f"example:{path.stem}" for path in (crate_dir / "examples").glob("*.rs"))
    if (crate_dir / "build.rs").is_file() or package.get("build"):
        found.add("custom-build:build-script-build")
    return found


def assert_label_exists(root: pathlib.Path, label: str) -> None:
    if not label.startswith("//") or ":" not in label:
        fail(f"invalid label {label!r}")
    package, target = label[2:].split(":", 1)
    build = root / package / "BUILD.bazel" if package else root / "BUILD.bazel"
    if not build.is_file():
        fail(f"{label} has no BUILD.bazel at {build}")
    pattern = re.compile(r'\bname\s*=\s*"' + re.escape(target) + r'"')
    if not pattern.search(build.read_text()):
        fail(f"{label} is not declared in {build.relative_to(root)}")


def main() -> None:
    if len(sys.argv) != 3:
        fail("expected INVENTORY ROOT_CARGO_TOML")
    inventory_path = pathlib.Path(sys.argv[1]).resolve()
    root_manifest = pathlib.Path(sys.argv[2]).resolve()
    root = root_manifest.parent
    inventory = json.loads(inventory_path.read_text())
    if inventory.get("schema_version") != 1:
        fail("unsupported schema_version")
    packages = inventory.get("packages", {})
    workspace = tomllib.loads(root_manifest.read_text())["workspace"]
    manifests = [root / member / "Cargo.toml" for member in workspace["members"]]
    cargo_names = {tomllib.loads(path.read_text())["package"]["name"] for path in manifests}
    if set(packages) != cargo_names:
        fail(f"package mismatch: inventory={sorted(packages)} Cargo={sorted(cargo_names)}")
    for manifest in manifests:
        package_name = tomllib.loads(manifest.read_text())["package"]["name"]
        entry = packages[package_name]
        expected_manifest = manifest.relative_to(root).as_posix()
        if entry.get("manifest") != expected_manifest:
            fail(f"{package_name} manifest should be {expected_manifest}")
        actual = discover(manifest)
        declared = set(entry.get("targets", {}))
        if declared != actual:
            fail(f"{package_name} target mismatch: missing={sorted(actual-declared)} stale={sorted(declared-actual)}")
        labels = list(entry["targets"].values())
        for key in ("native_unit",):
            if entry.get(key):
                labels.append(entry[key])
        for key in ("feature_variants", "focused_tests"):
            labels.extend(entry.get(key, []))
        for label in labels:
            assert_label_exists(root, label)
    print(f"native Bazel inventory owns {sum(len(v['targets']) for v in packages.values())} Cargo targets across {len(packages)} packages")


if __name__ == "__main__":
    main()
