#!/usr/bin/env python3
"""Generate or strictly verify deterministic CycloneDX release SBOMs."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import tomllib
from typing import Any


HEX_40 = re.compile(r"[0-9a-f]{40}")
HEX_64 = re.compile(r"[0-9a-f]{64}")
VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)")
BUILD_INFO_CLASSIFICATION = "sanitized-release-evidence-not-slsa-provenance"


def regular_bytes(path: Path, label: str, maximum: int) -> bytes:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise ValueError(f"{label} is unavailable: {path}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise ValueError(f"{label} is not a regular file: {path}")
    if metadata.st_size <= 0 or metadata.st_size > maximum:
        raise ValueError(f"{label} has an invalid size: {path}")
    try:
        return path.read_bytes()
    except OSError as error:
        raise ValueError(f"{label} could not be read: {path}") from error


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path, label: str, maximum: int) -> str:
    metadata = path.lstat()
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise ValueError(f"{label} is not a regular file: {path}")
    if metadata.st_size <= 0 or metadata.st_size > maximum:
        raise ValueError(f"{label} has an invalid size: {path}")
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def material_ref(ecosystem: str, identity: str) -> str:
    digest = hashlib.sha256(f"{ecosystem}\0{identity}".encode()).hexdigest()
    return f"urn:ctx:material:{ecosystem}:{digest}"


def property_value(value: object) -> str:
    if isinstance(value, str):
        return value
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def properties(values: dict[str, object]) -> list[dict[str, str]]:
    return [
        {"name": name, "value": property_value(value)}
        for name, value in sorted(values.items())
        if value is not None and value != ""
    ]


def parse_dependency(
    value: str, packages: list[dict[str, Any]], refs: dict[tuple[str, str, str], str]
) -> str:
    match = re.fullmatch(r"([^ ]+)(?: ([^ ()]+))?(?: \((.+)\))?", value)
    if match is None:
        raise ValueError(f"Cargo.lock dependency is malformed: {value}")
    name, version, source = match.groups()
    candidates = [
        package
        for package in packages
        if package.get("name") == name
        and (version is None or package.get("version") == version)
        and (source is None or package.get("source") == source)
    ]
    if len(candidates) != 1:
        raise ValueError(f"Cargo.lock dependency is ambiguous: {value}")
    package = candidates[0]
    return refs[(package["name"], package["version"], package.get("source", ""))]


def package_identity(package: dict[str, Any]) -> tuple[str, str, str]:
    return (
        package["name"],
        package["version"],
        package.get("source", ""),
    )


def target_package_identities(
    inventory_bytes: bytes,
    packages: list[dict[str, Any]],
    root_package: str,
) -> set[tuple[str, str, str]]:
    try:
        labels = inventory_bytes.decode().splitlines()
    except UnicodeDecodeError as error:
        raise ValueError("target dependency inventory is malformed") from error
    if (
        not labels
        or labels != sorted(set(labels))
        or any(not label or any(character.isspace() for character in label) for label in labels)
    ):
        raise ValueError(
            "target dependency inventory must contain sorted unique Bazel labels"
        )

    selected: set[tuple[str, str, str]] = set()
    by_crate_repository: dict[str, list[tuple[str, str, str]]] = {}
    workspace_by_name: dict[str, list[tuple[str, str, str]]] = {}
    for package in packages:
        identity = package_identity(package)
        name, version, source = identity
        if source:
            by_crate_repository.setdefault(f"crates__{name}-{version}", []).append(
                identity
            )
        else:
            workspace_by_name.setdefault(name, []).append(identity)

    for label in labels:
        repository_match = re.search(r"(?:^|~)(crates__[^/]+)//", label)
        if repository_match is not None:
            candidates = by_crate_repository.get(repository_match.group(1), [])
            if len(candidates) != 1:
                raise ValueError(
                    f"target inventory Cargo repository is ambiguous: {label}"
                )
            selected.add(candidates[0])
            continue
        workspace_match = re.match(r"(?:@@?ctx_search)?//crates/([^/:]+)", label)
        if workspace_match is not None:
            directory = workspace_match.group(1)
            name = root_package if directory == "ctx-cli" else directory
            candidates = workspace_by_name.get(name, [])
            if len(candidates) != 1:
                raise ValueError(
                    f"target inventory workspace package is ambiguous: {label}"
                )
            selected.add(candidates[0])

    roots = [
        identity
        for identity in selected
        if identity[0] == root_package and identity[2] == ""
    ]
    if len(roots) != 1:
        raise ValueError(
            f"target dependency inventory must select one workspace {root_package} package"
        )
    return selected


def cargo_materials(
    lock_bytes: bytes,
    root_package: str,
    inventory_bytes: bytes,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], str]:
    try:
        value = tomllib.loads(lock_bytes.decode())
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise ValueError("Cargo.lock is malformed") from error
    packages = value.get("package")
    if not isinstance(packages, list) or not packages:
        raise ValueError("Cargo.lock contains no packages")

    selected = target_package_identities(inventory_bytes, packages, root_package)
    refs: dict[tuple[str, str, str], str] = {}
    components: list[dict[str, Any]] = []
    for package in packages:
        if not isinstance(package, dict):
            raise ValueError("Cargo.lock package is malformed")
        name = package.get("name")
        version = package.get("version")
        source = package.get("source", "")
        checksum = package.get("checksum")
        if (
            not isinstance(name, str)
            or not name
            or not isinstance(version, str)
            or not version
            or not isinstance(source, str)
            or (checksum is not None and not isinstance(checksum, str))
        ):
            raise ValueError("Cargo.lock package identity is malformed")
        identity = (name, version, source)
        if identity not in selected:
            continue
        if identity in refs:
            raise ValueError(f"Cargo.lock contains duplicate package {name} {version}")
        ref = material_ref("cargo", "\0".join(identity))
        refs[identity] = ref
        component: dict[str, Any] = {
            "type": "library",
            "bom-ref": ref,
            "name": name,
            "version": version,
            "properties": properties(
                {
                    "ctx:dependency:ecosystem": "cargo",
                    "ctx:dependency:source": source or "workspace",
                }
            ),
        }
        if checksum is not None:
            if HEX_64.fullmatch(checksum) is None:
                raise ValueError(f"Cargo.lock checksum is invalid for {name} {version}")
            component["hashes"] = [{"alg": "SHA-256", "content": checksum}]
        components.append(component)

    dependencies = []
    for package in packages:
        identity = (package["name"], package["version"], package.get("source", ""))
        if identity not in selected:
            continue
        package_dependencies = package.get("dependencies", [])
        if not isinstance(package_dependencies, list) or not all(
            isinstance(item, str) for item in package_dependencies
        ):
            raise ValueError("Cargo.lock package dependencies are malformed")
        dependency_refs = []
        for item in package_dependencies:
            try:
                dependency_refs.append(parse_dependency(item, packages, refs))
            except KeyError as error:
                raise ValueError(
                    f"target dependency inventory omits a dependency of "
                    f"{identity[0]} {identity[1]}: {item}"
                ) from error
        dependencies.append({"ref": refs[identity], "dependsOn": sorted(dependency_refs)})

    roots = [
        refs[(package["name"], package["version"], package.get("source", ""))]
        for package in packages
        if package_identity(package) in selected
        and package.get("name") == root_package
        and package.get("source") is None
    ]
    if len(roots) != 1:
        raise ValueError(f"Cargo.lock must contain one workspace {root_package} package")
    return components, dependencies, roots[0]


def load_core_build_info(
    build_info_bytes: bytes, artifact_sha256: str, cargo_lock_sha256: str, platform: str
) -> tuple[dict[str, object], dict[str, object]]:
    try:
        value = json.loads(build_info_bytes)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("Core build-info is malformed") from error
    source = value.get("source") if isinstance(value, dict) else None
    if (
        not isinstance(value, dict)
        or value.get("schema_version") != 1
        or value.get("artifact_sha256") != artifact_sha256
        or value.get("cargo_lock_sha256") != cargo_lock_sha256
        or value.get("platform") != platform
        or not isinstance(value.get("target"), str)
        or not value["target"]
        or not isinstance(value.get("rust_version"), str)
        or not value["rust_version"]
        or not isinstance(source, dict)
        or source.get("clean") is not True
        or not isinstance(source.get("commit"), str)
        or HEX_40.fullmatch(source["commit"]) is None
        or source["commit"] == "0" * 40
    ):
        raise ValueError("Core build-info does not bind the clean exact candidate")
    builder = value.get("builder")
    base_image = builder.get("base_image") if isinstance(builder, dict) else None
    facts: dict[str, object] = {
        "ctx:build-info:classification": BUILD_INFO_CLASSIFICATION,
        "ctx:build-info:sha256": sha256_bytes(build_info_bytes),
        "ctx:builder:base-image": (
            base_image.get("actual") if isinstance(base_image, dict) else None
        ),
        "ctx:builder:image-id": (
            builder.get("image_id") if isinstance(builder, dict) else None
        ),
        "ctx:builder:recipe-sha256": (
            builder.get("recipe_sha256") if isinstance(builder, dict) else None
        ),
        "ctx:builder:runtime-image-id": (
            value.get("runtime", {}).get("image_id")
            if isinstance(value.get("runtime"), dict)
            else None
        ),
        "ctx:builder:inspector-image-id": (
            value.get("inspector", {}).get("image_id")
            if isinstance(value.get("inspector"), dict)
            else None
        ),
        "ctx:platform": platform,
        "ctx:source:public-commit": source["commit"],
        "ctx:target": value["target"],
        "ctx:toolchain:rust-version": value.get("rust_version"),
    }
    return value, facts


def build_document(args: argparse.Namespace) -> bytes:
    if VERSION.fullmatch(args.version) is None:
        raise ValueError("release version must be canonical numeric semver")
    artifact_sha256 = sha256_file(
        args.artifact, "Core artifact", 256 * 1024 * 1024
    )
    build_info_bytes = regular_bytes(args.build_info, "build-info", 64 * 1024)
    cargo_lock_bytes = regular_bytes(args.cargo_lock, "Cargo.lock", 4 * 1024 * 1024)
    module_lock_bytes = regular_bytes(
        args.module_lock, "MODULE.bazel.lock", 4 * 1024 * 1024
    )
    module_bytes = regular_bytes(args.module_file, "MODULE.bazel", 256 * 1024)
    inventory_bytes = regular_bytes(
        args.target_inventory,
        "target dependency inventory",
        4 * 1024 * 1024,
    )
    cargo_lock_sha256 = sha256_bytes(cargo_lock_bytes)
    module_lock_sha256 = sha256_bytes(module_lock_bytes)
    module_sha256 = sha256_bytes(module_bytes)
    inventory_sha256 = sha256_bytes(inventory_bytes)

    root_name = "ctx"
    _, build_properties = load_core_build_info(
        build_info_bytes, artifact_sha256, cargo_lock_sha256, args.platform
    )

    cargo_components, cargo_dependencies, cargo_root = cargo_materials(
        cargo_lock_bytes, root_name, inventory_bytes
    )
    cargo_lock_ref = material_ref("file", f"Cargo.lock\0{cargo_lock_sha256}")
    module_lock_ref = material_ref(
        "file", f"MODULE.bazel.lock\0{module_lock_sha256}"
    )
    inventory_ref = material_ref(
        "file", f"target-dependency-inventory.txt\0{inventory_sha256}"
    )
    file_components = [
        {
            "type": "file",
            "bom-ref": cargo_lock_ref,
            "name": "Cargo.lock",
            "hashes": [{"alg": "SHA-256", "content": cargo_lock_sha256}],
        },
        {
            "type": "file",
            "bom-ref": module_lock_ref,
            "name": "MODULE.bazel.lock",
            "hashes": [{"alg": "SHA-256", "content": module_lock_sha256}],
        },
        {
            "type": "file",
            "bom-ref": inventory_ref,
            "name": "target-dependency-inventory.txt",
            "hashes": [{"alg": "SHA-256", "content": inventory_sha256}],
        },
    ]
    root_ref = f"urn:ctx:artifact:sha256:{artifact_sha256}"
    root_component = {
        "type": "application",
        "bom-ref": root_ref,
        "name": root_name,
        "version": args.version,
        "hashes": [{"alg": "SHA-256", "content": artifact_sha256}],
    }
    direct_dependencies = [
        cargo_root,
        cargo_lock_ref,
        module_lock_ref,
        inventory_ref,
    ]
    all_components = cargo_components + file_components
    all_components.sort(
        key=lambda item: (
            item["type"],
            item["name"],
            item.get("version", ""),
            item["bom-ref"],
        )
    )
    all_dependencies = cargo_dependencies + [
        {"ref": component["bom-ref"], "dependsOn": []}
        for component in file_components
    ]
    all_dependencies.append(
        {"ref": root_ref, "dependsOn": sorted(direct_dependencies)}
    )
    all_dependencies.sort(key=lambda item: item["ref"])

    build_properties.update(
        {
            "ctx:dependency:cargo-lock-sha256": cargo_lock_sha256,
            "ctx:dependency:module-file-sha256": module_sha256,
            "ctx:dependency:module-lock-sha256": module_lock_sha256,
            "ctx:dependency:target-inventory-sha256": inventory_sha256,
            "ctx:document:classification": "target-exact-artifact-sbom",
            "ctx:document:deterministic": "true",
        }
    )
    document = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "version": 1,
        "metadata": {
            "component": root_component,
            "properties": properties(build_properties),
        },
        "components": all_components,
        "dependencies": all_dependencies,
    }
    return canonical(document)


def atomic_write(path: Path, payload: bytes) -> None:
    temporary = path.with_name(f".{path.name}.tmp.{os.getpid()}")
    try:
        temporary.write_bytes(payload)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=("generate", "verify"))
    parser.add_argument("--product", choices=("core",), required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--build-info", type=Path, required=True)
    parser.add_argument("--cargo-lock", type=Path, required=True)
    parser.add_argument("--module-lock", type=Path, required=True)
    parser.add_argument("--module-file", type=Path, required=True)
    parser.add_argument("--target-inventory", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--sbom", type=Path)
    args = parser.parse_args()
    try:
        expected = build_document(args)
        if args.mode == "generate":
            if args.output is None or args.sbom is not None:
                parser.error("generate requires --output and forbids --sbom")
            atomic_write(args.output, expected)
            print(sha256_bytes(expected))
        else:
            if args.sbom is None or args.output is not None:
                parser.error("verify requires --sbom and forbids --output")
            actual = regular_bytes(args.sbom, "CycloneDX SBOM", 8 * 1024 * 1024)
            try:
                json.loads(actual)
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                raise ValueError("CycloneDX SBOM is malformed") from error
            if actual != expected:
                raise ValueError(
                    "CycloneDX SBOM does not match the exact artifact, source, "
                    "build, and dependency material"
                )
            print(sha256_bytes(actual))
    except (OSError, ValueError) as error:
        raise SystemExit(f"error: {error}") from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
