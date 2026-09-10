#!/usr/bin/env python3
"""Write target-exact release dependency inputs from locked Cargo metadata."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
from pathlib import PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
from typing import Any

sys.dont_write_bytecode = True
sys.path.insert(0, os.fspath(Path(__file__).resolve().parent))
import notify_source


NOTICE_NAMES = re.compile(r"^(?:authors|copying|licen[cs]e|notice|unlicense)", re.I)
REQUIRED_TANTIVY_FEATURES = {
    "columnar-zstd-compression",
    "fs4",
    "lz4-compression",
    "lz4_flex",
    "memmap2",
    "mmap",
    "tempfile",
    "zstd",
    "zstd-compression",
}


def canonical(value: object) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n"


def run_metadata(repo: Path, target: str, patched_notify: Path | None = None) -> dict[str, Any]:
    result = subprocess.run(
        [
            "cargo",
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            target,
            *(["--config", os.fspath(patched_notify.parent / "config.toml")]
              if patched_notify is not None else []),
        ],
        cwd=repo,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise ValueError(result.stderr.strip() or "cargo metadata failed")
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ValueError("cargo metadata returned malformed JSON") from error
    if not isinstance(value, dict) or not isinstance(value.get("packages"), list):
        raise ValueError("cargo metadata returned an invalid package graph")
    if patched_notify is not None:
        notify_source.bind_metadata(value, patched_notify)
    return value


def selected_package_ids(metadata: dict[str, Any]) -> set[str]:
    packages = metadata["packages"]
    roots = [
        package["id"]
        for package in packages
        if package.get("name") == "ctx" and package.get("source") is None
    ]
    resolve = metadata.get("resolve")
    nodes = resolve.get("nodes") if isinstance(resolve, dict) else None
    if len(roots) != 1 or not isinstance(nodes, list):
        raise ValueError("target graph must contain one workspace ctx package")
    by_id = {node.get("id"): node for node in nodes if isinstance(node, dict)}
    selected = {roots[0]}
    pending = [roots[0]]
    while pending:
        node = by_id.get(pending.pop())
        if not isinstance(node, dict):
            raise ValueError("target graph is missing a resolved package node")
        dependencies = node.get("deps")
        if not isinstance(dependencies, list):
            raise ValueError("target graph contains a malformed dependency list")
        for dependency in dependencies:
            package_id = dependency.get("pkg") if isinstance(dependency, dict) else None
            if not isinstance(package_id, str):
                raise ValueError("target graph contains a malformed dependency")
            if package_id not in selected:
                selected.add(package_id)
                pending.append(package_id)
    return selected


def package_label(package: dict[str, Any], repo: Path) -> str:
    manifest = Path(package["manifest_path"])
    if package.get("source") is None:
        relative = manifest.parent.resolve().relative_to(repo.resolve()).as_posix()
        return f"@@//{relative}:{'ctx' if package['name'] == 'ctx' else 'lib'}"
    version = str(package["version"]).replace("+", "-")
    return f"@@rules_rust++crate+crates__{package['name']}-{version}//:{package['name']}"


def logical_external_root(package: dict[str, Any]) -> str:
    version = str(package["version"]).replace("+", "-")
    return f"rules_rust++crate+crates__{package['name']}-{version}"


def safe_materials(package: dict[str, Any], repo: Path) -> list[dict[str, str]]:
    manifest = Path(package["manifest_path"]).resolve()
    root = manifest.parent
    if package.get("source") is None:
        relative_root = root.relative_to(repo.resolve()).as_posix()
        logical_root = "" if relative_root == "." else relative_root
        kind = "main"
    else:
        logical_root = logical_external_root(package)
        kind = "external"
    paths = [manifest]
    paths.extend(
        path
        for path in root.iterdir()
        if path.is_file() and NOTICE_NAMES.match(path.name)
    )
    records = []
    for path in sorted(set(paths), key=lambda item: item.name.lower()):
        logical = f"{logical_root}/{path.name}" if logical_root else path.name
        records.append({"kind": kind, "logical": logical, "path": os.fspath(path)})
    return records


def canonical_material_records(records: list[dict[str, str]]) -> list[dict[str, str]]:
    unique: dict[tuple[str, str], dict[str, str]] = {}
    for record in records:
        kind = record.get("kind")
        logical = record.get("logical")
        if not isinstance(kind, str) or not isinstance(logical, str):
            raise ValueError("release material record is malformed")
        path = PurePosixPath(logical)
        normalized = path.as_posix()
        logical = normalized
        if (
            kind not in {"main", "external"}
            or not logical
            or path.is_absolute()
            or ".." in path.parts
        ):
            raise ValueError("release material path is unsafe")
        normalized_record = {"kind": kind, "logical": logical, "path": record["path"]}
        key = (kind, logical)
        previous = unique.get(key)
        if previous is not None:
            if Path(previous["path"]).resolve() != Path(record["path"]).resolve():
                raise ValueError(f"duplicate release material has conflicting sources: {logical}")
            continue
        unique[key] = normalized_record
    return [unique[key] for key in sorted(unique)]


def configured_features(
    metadata: dict[str, Any], selected: set[str], repo: Path
) -> list[dict[str, str]]:
    resolve = metadata["resolve"]
    records = []
    for node in resolve["nodes"]:
        if node["id"] not in selected:
            continue
        package = next(item for item in metadata["packages"] if item["id"] == node["id"])
        version = str(package["version"]).replace("+", "-")
        label = package_label(package, repo)
        for feature in node.get("features", []):
            records.append({"label": label, "feature": feature})
    tantivy = {
        record["feature"]
        for record in records
        if "crates__tantivy-0.26.1//:tantivy" in record["label"]
    }
    if tantivy != REQUIRED_TANTIVY_FEATURES:
        raise ValueError(
            "Cargo target graph does not have the pinned defaults-off Tantivy features"
        )
    return sorted(records, key=lambda item: (item["label"], item["feature"]))


def write_atomic(path: Path, payload: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp.{os.getpid()}")
    try:
        temporary.write_text(payload, encoding="utf-8")
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def stage_materials(
    records: list[dict[str, str]], material_root: Path
) -> list[dict[str, str]]:
    material_root.mkdir(parents=True, exist_ok=True)
    if any(material_root.iterdir()):
        raise ValueError("material root must be empty")
    portable = []
    for record in canonical_material_records(records):
        source = Path(record["path"])
        destination = material_root / record["logical"]
        try:
            source_metadata = source.lstat()
        except OSError as error:
            raise ValueError(f"release material is unavailable: {source}") from error
        if stat.S_ISLNK(source_metadata.st_mode) or not stat.S_ISREG(source_metadata.st_mode):
            raise ValueError(f"release material is not a regular file: {source}")
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
        portable.append({"kind": record["kind"], "logical": record["logical"]})
    return portable


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--target", required=True)
    parser.add_argument("--target-output", required=True, type=Path)
    parser.add_argument("--materials-output", required=True, type=Path)
    parser.add_argument("--material-root", required=True, type=Path)
    parser.add_argument("--notify-source", type=Path)
    args = parser.parse_args()
    try:
        repo = args.repo.resolve(strict=True)
        metadata = run_metadata(repo, args.target, args.notify_source)
        selected = selected_package_ids(metadata)
        packages = [item for item in metadata["packages"] if item["id"] in selected]
        package_records = sorted([
            {
                "label": package_label(package, repo),
                "name": package["name"],
                "source": package.get("source"),
                "version": package["version"],
            }
            for package in packages
        ], key=lambda item: item["label"])
        feature_records = configured_features(metadata, selected, repo)
        target_document = {
            "schema_version": 1,
            "kind": "ctx-cargo-release-inventory",
            "target": args.target,
            "packages": package_records,
            "features": feature_records,
        }
        if "source_patches" in metadata:
            target_document["source_patches"] = metadata["source_patches"]
        material_records = sorted([
            {
                "kind": "main",
                "logical": "Cargo.toml",
                "path": os.fspath(repo / "Cargo.toml"),
            },
            *[
                material
                for package in packages
                for material in safe_materials(package, repo)
            ],
        ], key=lambda item: (item["kind"], item["logical"]))
        materials_document = {
            "schema_version": 1,
            "kind": "ctx-cargo-release-inventory",
            "target": args.target,
            "features": feature_records,
            "materials": stage_materials(material_records, args.material_root),
        }
        write_atomic(args.target_output, canonical(target_document))
        write_atomic(args.materials_output, canonical(materials_document))
    except (OSError, ValueError) as error:
        raise SystemExit(f"error: {error}") from error
    print(args.target_output)
    print(args.materials_output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
