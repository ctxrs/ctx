#!/usr/bin/env python3
"""Generate and verify exact-byte public CLI release evidence bundles."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import sys
from typing import Any

_SCRIPT_DIRECTORY = os.fspath(Path(__file__).resolve().parent)
sys.path.insert(0, _SCRIPT_DIRECTORY)
try:
    from release_sbom.dependency_materials import (
        BUILD_INFO_CLASSIFICATION,
        HEX_40,
        HEX_64,
        Identity,
        TANTIVY_FEATURES,
        TANTIVY_RESOLVED_FEATURES,
        TANTIVY_VERSION,
        VERSION,
        assert_tantivy_contract,
        canonical,
        cargo_materials,
        material_ref,
        package_identity,
        package_metadata,
        parse_cargo_lock,
        properties,
        regular_bytes,
        selected_adjacency,
        sha256_bytes,
        sha256_file,
        tantivy_closure,
        target_package_identities,
    )
finally:
    del sys.path[0]
del _SCRIPT_DIRECTORY

def load_core_build_info(
    build_info_bytes: bytes,
    artifact_sha256: str,
    cargo_lock_sha256: str | None,
    platform: str,
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
        or (
            cargo_lock_sha256 is not None
            and value.get("cargo_lock_sha256") != cargo_lock_sha256
        )
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


def target_contract(
    matrix_bytes: bytes,
    target_id: str,
    platform: str,
    rust_target: str,
) -> dict[str, Any]:
    try:
        value = json.loads(matrix_bytes)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("release target matrix is malformed") from error
    targets = value.get("targets") if isinstance(value, dict) else None
    matches = (
        [
            target
            for target in targets
            if isinstance(target, dict) and target.get("id") == target_id
        ]
        if isinstance(targets, list)
        else []
    )
    expected_platform = "linux-aarch64" if target_id == "linux-arm64" else target_id
    if (
        value.get("schema_version") != 1
        or len(matches) != 1
        or platform != expected_platform
        or matches[0].get("public_rust_target") != rust_target
        or matches[0].get("public_construction_authority")
        != "bazel-release-route-v1"
        or matches[0].get("public_construction_label")
        != f"//:ctx_release_{target_id.replace('-', '_')}"
    ):
        raise ValueError("release target matrix does not bind the Bazel candidate route")
    return matches[0]


def third_party_notices(
    artifact_sha256: str,
    platform: str,
    target_id: str,
    version: str,
    selected: set[Identity],
    packages_by_identity: dict[Identity, dict[str, Any]],
    metadata: dict[Identity, dict[str, Any]],
) -> bytes:
    third_party = sorted(identity for identity in selected if identity[2])
    lines = [
        "ctx THIRD_PARTY_NOTICES",
        f"version: {version}",
        f"target: {target_id}",
        f"platform: {platform}",
        f"artifact_sha256: {artifact_sha256}",
        f"package_count: {len(third_party)}",
        "",
        "Packages",
        "========",
    ]
    texts: dict[str, dict[str, object]] = {}
    for identity in third_party:
        package = packages_by_identity[identity]
        details = metadata[identity]
        lines.extend(
            [
                "",
                f"{identity[0]} {identity[1]}",
                f"  license: {details['license']}",
                f"  source: {identity[2]}",
            ]
        )
        checksum = package.get("checksum")
        if checksum:
            lines.append(f"  checksum_sha256: {checksum}")
        for key in ("repository", "homepage"):
            value = details.get(key)
            if isinstance(value, str) and value:
                lines.append(f"  {key}: {value}")
        files = details["notice_files"]
        if files:
            lines.append(
                "  notice_files: "
                + ", ".join(
                    f"{item['logical']}@sha256:{item['sha256']}" for item in files
                )
            )
            for item in files:
                entry = texts.setdefault(
                    str(item["sha256"]),
                    {
                        "files": set(),
                        "packages": set(),
                        "text": item["text"],
                    },
                )
                entry["files"].add(str(item["logical"]))
                entry["packages"].add(f"{identity[0]} {identity[1]}")
        else:
            lines.append(
                "  notice_files: none in the published Cargo package; "
                "the license grant is the package license expression above"
            )
    lines.extend(["", "License and notice texts", "========================"])
    for digest in sorted(texts):
        entry = texts[digest]
        lines.extend(
            [
                "",
                f"sha256: {digest}",
                "packages: " + ", ".join(sorted(entry["packages"])),
                "files: " + ", ".join(sorted(entry["files"])),
                "----",
                str(entry["text"]).rstrip("\n"),
                "----",
            ]
        )
    return ("\n".join(lines) + "\n").encode()


def evidence_record(path: Path, payload: bytes) -> dict[str, str]:
    return {"file": path.name, "sha256": sha256_bytes(payload)}


def build_bundle(args: argparse.Namespace) -> dict[str, bytes]:
    if VERSION.fullmatch(args.version) is None:
        raise ValueError("release version must be canonical numeric semver")
    artifact_sha256 = sha256_file(
        args.artifact, "Core artifact", 256 * 1024 * 1024
    )
    artifact_size = args.artifact.stat().st_size
    build_info_bytes = regular_bytes(args.build_info, "build-info", 64 * 1024)
    cargo_lock_bytes = regular_bytes(args.cargo_lock, "Cargo.lock", 4 * 1024 * 1024)
    module_lock_bytes = regular_bytes(
        args.module_lock, "MODULE.bazel.lock", 16 * 1024 * 1024
    )
    module_bytes = regular_bytes(args.module_file, "MODULE.bazel", 256 * 1024)
    inventory_bytes = regular_bytes(
        args.target_inventory,
        "target dependency inventory",
        4 * 1024 * 1024,
    )
    material_inventory_bytes = regular_bytes(
        args.license_materials,
        "license material inventory",
        8 * 1024 * 1024,
    )
    matrix_bytes = regular_bytes(
        args.target_matrix, "release target matrix", 256 * 1024
    )
    schema_bytes = regular_bytes(
        args.candidate_schema, "candidate manifest schema", 256 * 1024
    )
    workspace_manifest_bytes = regular_bytes(
        args.workspace_manifest, "workspace Cargo.toml", 256 * 1024
    )
    index_manifest_bytes = regular_bytes(
        args.index_manifest, "ctx-history-index Cargo.toml", 256 * 1024
    )

    cargo_lock_sha256 = sha256_bytes(cargo_lock_bytes)
    build_info, build_properties = load_core_build_info(
        build_info_bytes, artifact_sha256, cargo_lock_sha256, args.platform
    )
    target = target_contract(
        matrix_bytes,
        args.target_id,
        args.platform,
        str(build_info["target"]),
    )
    packages = parse_cargo_lock(cargo_lock_bytes)
    packages_by_identity = {
        package_identity(package): package for package in packages
    }
    selected = target_package_identities(inventory_bytes, packages, "ctx")
    metadata, configured_features, material_inventory_sha256 = package_metadata(
        selected,
        material_inventory_bytes,
        args.runfiles_root,
    )
    tantivy = assert_tantivy_contract(
        workspace_manifest_bytes,
        index_manifest_bytes,
        packages,
        selected,
        configured_features,
    )
    adjacency = selected_adjacency(packages, selected)
    tantivy_packages, tantivy_closure_sha256 = tantivy_closure(
        tantivy,
        adjacency,
        packages_by_identity,
        metadata,
    )
    cargo_components, cargo_dependencies, cargo_root = cargo_materials(
        packages,
        selected,
        metadata,
        adjacency,
    )

    file_inputs = [
        ("Cargo.lock", cargo_lock_bytes),
        ("MODULE.bazel", module_bytes),
        ("MODULE.bazel.lock", module_lock_bytes),
        ("release-candidate-manifest-v1.schema.json", schema_bytes),
        ("release-targets-v1.json", matrix_bytes),
        ("target-dependency-inventory.txt", inventory_bytes),
        ("license-materials-inventory.txt", material_inventory_bytes),
        ("workspace-Cargo.toml", workspace_manifest_bytes),
        ("ctx-history-index-Cargo.toml", index_manifest_bytes),
    ]
    file_components = []
    direct_dependencies = [cargo_root]
    for name, payload in file_inputs:
        digest = sha256_bytes(payload)
        ref = material_ref("file", f"{name}\0{digest}")
        file_components.append(
            {
                "type": "file",
                "bom-ref": ref,
                "name": name,
                "hashes": [{"alg": "SHA-256", "content": digest}],
            }
        )
        direct_dependencies.append(ref)

    root_ref = f"urn:ctx:artifact:sha256:{artifact_sha256}"
    root_license = metadata[
        next(identity for identity in selected if identity[0] == "ctx" and not identity[2])
    ]["license"]
    root_component = {
        "type": "application",
        "bom-ref": root_ref,
        "name": "ctx",
        "version": args.version,
        "hashes": [{"alg": "SHA-256", "content": artifact_sha256}],
        "licenses": [{"expression": root_license}],
    }
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
            "ctx:construction:authority": target["public_construction_authority"],
            "ctx:construction:label": target["public_construction_label"],
            "ctx:dependency:cargo-lock-sha256": cargo_lock_sha256,
            "ctx:dependency:license-materials-sha256": material_inventory_sha256,
            "ctx:dependency:module-file-sha256": sha256_bytes(module_bytes),
            "ctx:dependency:module-lock-sha256": sha256_bytes(module_lock_bytes),
            "ctx:dependency:target-inventory-sha256": sha256_bytes(inventory_bytes),
            "ctx:document:classification": "target-exact-artifact-sbom",
            "ctx:document:deterministic": "true",
            "ctx:target-id": args.target_id,
            "ctx:tantivy:dependency-closure-sha256": tantivy_closure_sha256,
            "ctx:tantivy:features": TANTIVY_FEATURES,
            "ctx:tantivy:resolved-crate-features": TANTIVY_RESOLVED_FEATURES,
        }
    )
    sbom_bytes = canonical(
        {
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
    )
    notices_bytes = third_party_notices(
        artifact_sha256,
        args.platform,
        args.target_id,
        args.version,
        selected,
        packages_by_identity,
        metadata,
    )
    size_bytes = canonical(
        {
            "schema_version": 1,
            "kind": "ctx-binary-size-report",
            "product": "core",
            "version": args.version,
            "target": {
                "id": args.target_id,
                "platform": args.platform,
                "rust_triple": build_info["target"],
            },
            "artifact": {
                "file": args.artifact.name,
                "sha256": artifact_sha256,
                "size_bytes": artifact_size,
            },
        }
    )
    evidence = {
        "binary_size_report": evidence_record(args.size_report_output, size_bytes),
        "build_info": evidence_record(args.build_info, build_info_bytes),
        "candidate_schema": evidence_record(args.candidate_schema, schema_bytes),
        "cargo_lock": evidence_record(args.cargo_lock, cargo_lock_bytes),
        "ctx_history_index_manifest": evidence_record(
            args.index_manifest, index_manifest_bytes
        ),
        "cyclonedx_sbom": evidence_record(args.output, sbom_bytes),
        "license_materials_inventory": evidence_record(
            args.license_materials, material_inventory_bytes
        ),
        "module_file": evidence_record(args.module_file, module_bytes),
        "module_lock": evidence_record(args.module_lock, module_lock_bytes),
        "target_dependency_inventory": evidence_record(
            args.target_inventory, inventory_bytes
        ),
        "target_matrix": evidence_record(args.target_matrix, matrix_bytes),
        "third_party_notices": evidence_record(
            args.notices_output, notices_bytes
        ),
        "workspace_manifest": evidence_record(
            args.workspace_manifest, workspace_manifest_bytes
        ),
    }
    candidate_bytes = canonical(
        {
            "schema_version": 1,
            "kind": "ctx-public-cli-candidate",
            "construction": {
                "authority": target["public_construction_authority"],
                "label": target["public_construction_label"],
            },
            "product": "core",
            "version": args.version,
            "target": {
                "id": args.target_id,
                "platform": args.platform,
                "rust_triple": build_info["target"],
            },
            "source": build_info["source"],
            "artifact": {
                "file": args.artifact.name,
                "sha256": artifact_sha256,
                "size_bytes": artifact_size,
            },
            "evidence": evidence,
            "tantivy": {
                "version": TANTIVY_VERSION,
                "default_features": False,
                "features": TANTIVY_FEATURES,
                "resolved_crate_features": TANTIVY_RESOLVED_FEATURES,
                "dependency_closure": tantivy_packages,
                "dependency_closure_sha256": tantivy_closure_sha256,
            },
        }
    )
    return {
        "candidate": candidate_bytes,
        "notices": notices_bytes,
        "sbom": sbom_bytes,
        "size": size_bytes,
    }


def atomic_write(path: Path, payload: bytes) -> None:
    temporary = path.with_name(f".{path.name}.tmp.{os.getpid()}")
    try:
        temporary.write_bytes(payload)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def read_canonical_json(path: Path, label: str, maximum: int) -> tuple[dict[str, Any], bytes]:
    payload = regular_bytes(path, label, maximum)
    try:
        value = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{label} is malformed") from error
    if not isinstance(value, dict) or canonical(value) != payload:
        raise ValueError(f"{label} is not canonical JSON")
    return value, payload


def verify_bundle_only(args: argparse.Namespace) -> str:
    artifact_sha256 = sha256_file(
        args.artifact, "Core artifact", 256 * 1024 * 1024
    )
    artifact_size = args.artifact.stat().st_size
    candidate, candidate_bytes = read_canonical_json(
        args.candidate_manifest, "candidate manifest", 16 * 1024 * 1024
    )
    expected_top = {
        "schema_version",
        "kind",
        "construction",
        "product",
        "version",
        "target",
        "source",
        "artifact",
        "evidence",
        "tantivy",
    }
    if (
        set(candidate) != expected_top
        or candidate.get("schema_version") != 1
        or candidate.get("kind") != "ctx-public-cli-candidate"
        or candidate.get("product") != "core"
        or candidate.get("artifact")
        != {
            "file": args.artifact.name,
            "sha256": artifact_sha256,
            "size_bytes": artifact_size,
        }
        or candidate.get("construction", {}).get("authority")
        != "bazel-release-route-v1"
    ):
        raise ValueError("candidate manifest does not bind the exact Bazel artifact")
    target = candidate.get("target")
    if (
        not isinstance(target, dict)
        or set(target) != {"id", "platform", "rust_triple"}
        or not all(
            isinstance(target.get(name), str) and target[name]
            for name in ("id", "platform", "rust_triple")
        )
        or candidate.get("version") is None
        or VERSION.fullmatch(str(candidate["version"])) is None
        or target["platform"]
        != ("linux-aarch64" if target["id"] == "linux-arm64" else target["id"])
    ):
        raise ValueError("candidate manifest target is malformed")
    if candidate.get("construction", {}).get("label") != (
        f"//:ctx_release_{str(target['id']).replace('-', '_')}"
    ):
        raise ValueError("candidate manifest does not bind its target Bazel route")
    build_info_bytes = regular_bytes(args.build_info, "build-info", 64 * 1024)
    build_info, _ = load_core_build_info(
        build_info_bytes,
        artifact_sha256,
        None,
        str(target["platform"]),
    )
    if (
        candidate.get("source") != build_info["source"]
        or target["rust_triple"] != build_info["target"]
    ):
        raise ValueError("candidate manifest does not bind its exact build-info")
    evidence_paths = {
        "binary_size_report": args.size_report,
        "build_info": args.build_info,
        "cyclonedx_sbom": args.sbom,
        "third_party_notices": args.notices,
    }
    evidence = candidate.get("evidence")
    expected_evidence = {
        "binary_size_report",
        "build_info",
        "candidate_schema",
        "cargo_lock",
        "ctx_history_index_manifest",
        "cyclonedx_sbom",
        "license_materials_inventory",
        "module_file",
        "module_lock",
        "target_dependency_inventory",
        "target_matrix",
        "third_party_notices",
        "workspace_manifest",
    }
    if not isinstance(evidence, dict) or set(evidence) != expected_evidence:
        raise ValueError("candidate manifest evidence is malformed")
    for name, record in evidence.items():
        if (
            not isinstance(record, dict)
            or set(record) != {"file", "sha256"}
            or not isinstance(record.get("file"), str)
            or not record["file"]
            or HEX_64.fullmatch(str(record.get("sha256"))) is None
        ):
            raise ValueError(f"candidate manifest {name} evidence is malformed")
    for name, path in evidence_paths.items():
        record = evidence.get(name)
        payload = regular_bytes(path, name.replace("_", " "), 32 * 1024 * 1024)
        if record != {"file": path.name, "sha256": sha256_bytes(payload)}:
            raise ValueError(f"candidate manifest does not bind {name}")
    size_report, _ = read_canonical_json(
        args.size_report, "binary size report", 256 * 1024
    )
    if (
        set(size_report)
        != {"artifact", "kind", "product", "schema_version", "target", "version"}
        or size_report.get("schema_version") != 1
        or size_report.get("kind") != "ctx-binary-size-report"
        or size_report.get("product") != candidate["product"]
        or size_report.get("version") != candidate["version"]
        or size_report.get("target") != target
        or size_report.get("artifact") != candidate["artifact"]
    ):
        raise ValueError("binary size report does not bind the exact artifact")
    sbom, _ = read_canonical_json(args.sbom, "CycloneDX SBOM", 16 * 1024 * 1024)
    sbom_root = sbom.get("metadata", {}).get("component", {})
    if (
        sbom.get("bomFormat") != "CycloneDX"
        or sbom_root.get("name") != "ctx"
        or sbom_root.get("version") != candidate["version"]
        or sbom_root.get("hashes")
        != [{"alg": "SHA-256", "content": artifact_sha256}]
    ):
        raise ValueError("CycloneDX SBOM does not bind the exact artifact")
    notices = regular_bytes(
        args.notices, "third-party notices", 32 * 1024 * 1024
    )
    notice_bindings = (
        f"version: {candidate['version']}\n",
        f"target: {target['id']}\n",
        f"platform: {target['platform']}\n",
        f"artifact_sha256: {artifact_sha256}\n",
    )
    if any(binding.encode() not in notices for binding in notice_bindings):
        raise ValueError("third-party notices do not bind the exact artifact")
    tantivy = candidate.get("tantivy")
    closure = tantivy.get("dependency_closure") if isinstance(tantivy, dict) else None
    closure_identities: list[tuple[str, str, str]] = []
    if isinstance(closure, list):
        for package in closure:
            allowed = {"checksum", "license", "name", "source", "version"}
            if (
                not isinstance(package, dict)
                or not {"license", "name", "source", "version"}.issubset(package)
                or not set(package).issubset(allowed)
                or not all(
                    isinstance(package.get(name), str) and package[name]
                    for name in ("license", "name", "source", "version")
                )
                or (
                    "checksum" in package
                    and HEX_64.fullmatch(str(package["checksum"])) is None
                )
            ):
                raise ValueError("candidate manifest Tantivy closure is malformed")
            closure_identities.append(
                (package["name"], package["version"], package["source"])
            )
    if (
        not isinstance(tantivy, dict)
        or tantivy.get("version") != TANTIVY_VERSION
        or tantivy.get("default_features") is not False
        or tantivy.get("features") != TANTIVY_FEATURES
        or tantivy.get("resolved_crate_features") != TANTIVY_RESOLVED_FEATURES
        or HEX_64.fullmatch(str(tantivy.get("dependency_closure_sha256"))) is None
        or not closure_identities
        or closure_identities != sorted(set(closure_identities))
        or sha256_bytes(canonical(closure))
        != tantivy.get("dependency_closure_sha256")
        or ("tantivy", TANTIVY_VERSION)
        not in {(name, version) for name, version, _ in closure_identities}
        or {"fs4", "lz4_flex", "memmap2", "tempfile", "zstd"}
        - {name for name, _, _ in closure_identities}
        or any(name == "rust-stemmers" for name, _, _ in closure_identities)
    ):
        raise ValueError("candidate manifest Tantivy contract is malformed")
    return sha256_bytes(candidate_bytes)


def require_full_arguments(parser: argparse.ArgumentParser, args: argparse.Namespace) -> None:
    names = (
        "artifact",
        "build_info",
        "candidate_manifest",
        "candidate_schema",
        "cargo_lock",
        "index_manifest",
        "license_materials",
        "module_file",
        "module_lock",
        "notices",
        "notices_output",
        "output",
        "platform",
        "product",
        "sbom",
        "size_report",
        "size_report_output",
        "target_id",
        "target_inventory",
        "target_matrix",
        "version",
        "workspace_manifest",
    )
    generate_only = {"notices_output", "output", "size_report_output"}
    verify_only = {"notices", "sbom", "size_report"}
    missing = []
    for name in names:
        if args.mode == "generate" and name in verify_only:
            continue
        if args.mode == "verify" and name in generate_only:
            continue
        if getattr(args, name) is None:
            missing.append("--" + name.replace("_", "-"))
    if missing:
        parser.error(f"{args.mode} requires " + ", ".join(missing))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=("generate", "verify", "verify-bundle"))
    parser.add_argument("--product", choices=("core",))
    parser.add_argument("--version")
    parser.add_argument("--target-id")
    parser.add_argument("--platform")
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--build-info", type=Path)
    parser.add_argument("--cargo-lock", type=Path)
    parser.add_argument("--module-lock", type=Path)
    parser.add_argument("--module-file", type=Path)
    parser.add_argument("--target-inventory", type=Path)
    parser.add_argument("--license-materials", type=Path)
    parser.add_argument("--target-matrix", type=Path)
    parser.add_argument("--candidate-schema", type=Path)
    parser.add_argument("--workspace-manifest", type=Path)
    parser.add_argument("--index-manifest", type=Path)
    parser.add_argument("--runfiles-root", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--notices-output", type=Path)
    parser.add_argument("--size-report-output", type=Path)
    parser.add_argument("--candidate-manifest", type=Path)
    parser.add_argument("--sbom", type=Path)
    parser.add_argument("--notices", type=Path)
    parser.add_argument("--size-report", type=Path)
    args = parser.parse_args()
    try:
        if args.mode == "verify-bundle":
            required = (
                "artifact",
                "build_info",
                "candidate_manifest",
                "notices",
                "sbom",
                "size_report",
            )
            missing = [
                "--" + name.replace("_", "-")
                for name in required
                if getattr(args, name) is None
            ]
            if missing:
                parser.error("verify-bundle requires " + ", ".join(missing))
            print(verify_bundle_only(args))
            return 0

        require_full_arguments(parser, args)
        if args.mode == "generate":
            outputs = (
                args.output,
                args.notices_output,
                args.size_report_output,
                args.candidate_manifest,
            )
            if len(set(outputs)) != len(outputs):
                parser.error("generate outputs must be distinct")
            bundle = build_bundle(args)
            atomic_write(args.output, bundle["sbom"])
            atomic_write(args.notices_output, bundle["notices"])
            atomic_write(args.size_report_output, bundle["size"])
            atomic_write(args.candidate_manifest, bundle["candidate"])
            print(sha256_bytes(bundle["candidate"]))
        else:
            args.output = args.sbom
            args.notices_output = args.notices
            args.size_report_output = args.size_report
            bundle = build_bundle(args)
            actual = {
                "candidate": regular_bytes(
                    args.candidate_manifest,
                    "candidate manifest",
                    16 * 1024 * 1024,
                ),
                "notices": regular_bytes(
                    args.notices,
                    "third-party notices",
                    32 * 1024 * 1024,
                ),
                "sbom": regular_bytes(
                    args.sbom,
                    "CycloneDX SBOM",
                    16 * 1024 * 1024,
                ),
                "size": regular_bytes(
                    args.size_report,
                    "binary size report",
                    256 * 1024,
                ),
            }
            mismatched = [name for name in bundle if actual[name] != bundle[name]]
            if mismatched:
                raise ValueError(
                    "release evidence does not match the exact artifact, source, "
                    "build, license, feature, and dependency material: "
                    + ", ".join(sorted(mismatched))
                )
            print(sha256_bytes(actual["candidate"]))
    except (OSError, ValueError) as error:
        raise SystemExit(f"error: {error}") from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
