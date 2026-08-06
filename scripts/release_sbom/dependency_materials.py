"""Dependency and license-material assembly for release evidence bundles."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
from typing import Any
from urllib.parse import quote

try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib


HEX_40 = re.compile(r"[0-9a-f]{40}")
HEX_64 = re.compile(r"[0-9a-f]{64}")
VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)")
BUILD_INFO_CLASSIFICATION = "sanitized-release-evidence-not-slsa-provenance"
TANTIVY_VERSION = "0.26.1"
TANTIVY_FEATURES = [
    "columnar-zstd-compression",
    "lz4-compression",
    "mmap",
    "zstd-compression",
]
TANTIVY_RESOLVED_FEATURES = [
    "columnar-zstd-compression",
    "fs4",
    "lz4-compression",
    "lz4_flex",
    "memmap2",
    "mmap",
    "tempfile",
    "zstd",
    "zstd-compression",
]
WORKSPACE_RELEASE_PACKAGES = {
    "ctx-history-core",
    "ctx-history-index",
}
NOTICE_BASENAMES = (
    "authors",
    "copying",
    "licence",
    "license",
    "notice",
    "unlicense",
)


Identity = tuple[str, str, str]


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


def resolved_regular_bytes(path: Path, label: str, maximum: int) -> bytes:
    try:
        resolved = path.resolve(strict=True)
        metadata = resolved.stat()
    except OSError as error:
        raise ValueError(f"{label} is unavailable: {path}") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise ValueError(f"{label} is not a regular file: {path}")
    if metadata.st_size <= 0 or metadata.st_size > maximum:
        raise ValueError(f"{label} has an invalid size: {path}")
    try:
        return resolved.read_bytes()
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


def package_identity(package: dict[str, Any]) -> Identity:
    return (
        package["name"],
        package["version"],
        package.get("source", ""),
    )


def parse_dependency_identity(value: str, packages: list[dict[str, Any]]) -> Identity:
    match = re.fullmatch(r"([^ ]+)(?: ([^ ()]+))?(?: \((.+)\))?", value)
    if match is None:
        raise ValueError(f"Cargo.lock dependency is malformed: {value}")
    name, version, source = match.groups()
    candidates = [
        package_identity(package)
        for package in packages
        if package.get("name") == name
        and (version is None or package.get("version") == version)
        and (source is None or package.get("source") == source)
    ]
    if len(candidates) != 1:
        raise ValueError(f"Cargo.lock dependency is ambiguous: {value}")
    return candidates[0]


def parse_cargo_lock(lock_bytes: bytes) -> list[dict[str, Any]]:
    try:
        value = tomllib.loads(lock_bytes.decode())
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise ValueError("Cargo.lock is malformed") from error
    packages = value.get("package")
    if not isinstance(packages, list) or not packages:
        raise ValueError("Cargo.lock contains no packages")
    identities: set[Identity] = set()
    for package in packages:
        if not isinstance(package, dict):
            raise ValueError("Cargo.lock package is malformed")
        name = package.get("name")
        version = package.get("version")
        source = package.get("source", "")
        checksum = package.get("checksum")
        dependencies = package.get("dependencies", [])
        if (
            not isinstance(name, str)
            or not name
            or not isinstance(version, str)
            or not version
            or not isinstance(source, str)
            or (checksum is not None and not isinstance(checksum, str))
            or not isinstance(dependencies, list)
            or not all(isinstance(item, str) for item in dependencies)
        ):
            raise ValueError("Cargo.lock package identity or dependencies are malformed")
        identity = package_identity(package)
        if identity in identities:
            raise ValueError(f"Cargo.lock contains duplicate package {name} {version}")
        identities.add(identity)
        if checksum is not None and HEX_64.fullmatch(checksum) is None:
            raise ValueError(f"Cargo.lock checksum is invalid for {name} {version}")
    return packages


def target_package_identities(
    inventory_bytes: bytes,
    packages: list[dict[str, Any]],
    root_package: str,
) -> set[Identity]:
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

    selected: set[Identity] = set()
    by_crate_repository: dict[str, list[Identity]] = {}
    workspace_by_name: dict[str, list[Identity]] = {}
    for package in packages:
        identity = package_identity(package)
        name, version, source = identity
        if source:
            repository_version = version.replace("+", "-")
            by_crate_repository.setdefault(
                f"crates__{name}-{repository_version}", []
            ).append(identity)
        else:
            workspace_by_name.setdefault(name, []).append(identity)

    for label in labels:
        repository_match = re.search(r"(?:^|[~+])(crates__[^/]+)//", label)
        if repository_match is not None:
            candidates = by_crate_repository.get(repository_match.group(1), [])
            if len(candidates) != 1:
                raise ValueError(
                    f"target inventory Cargo repository is ambiguous: {label}"
                )
            selected.add(candidates[0])
            continue
        workspace_match = re.match(
            r"(?:(?:@@?ctx_search)|@@)?//crates/([^/:]+)",
            label,
        )
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
    missing_workspace = WORKSPACE_RELEASE_PACKAGES - {
        identity[0] for identity in selected if identity[2] == ""
    }
    if missing_workspace:
        raise ValueError(
            "target dependency inventory omits release workspace packages: "
            + ", ".join(sorted(missing_workspace))
        )
    return selected


def parse_material_inventory(
    inventory_bytes: bytes,
) -> tuple[list[tuple[str, str]], dict[str, set[str]]]:
    try:
        lines = inventory_bytes.decode().splitlines()
    except UnicodeDecodeError as error:
        raise ValueError("license material inventory is malformed") from error
    if not lines or lines != sorted(set(lines)):
        raise ValueError("license material inventory must be sorted and unique")
    files: list[tuple[str, str]] = []
    features: dict[str, set[str]] = {}
    for line in lines:
        fields = line.split("\t")
        if len(fields) == 2 and fields[0] in {"external", "main"}:
            kind, logical = fields
            path = PurePosixPath(logical)
            if (
                not logical
                or path.is_absolute()
                or ".." in path.parts
                or str(path) != logical
            ):
                raise ValueError(f"license material path is unsafe: {logical}")
            if kind == "external" and len(path.parts) < 2:
                raise ValueError(f"external license material path is invalid: {logical}")
            files.append((kind, logical))
        elif len(fields) == 3 and fields[0] == "feature":
            _, label, feature = fields
            if (
                not label
                or not feature
                or any(character.isspace() for character in feature)
            ):
                raise ValueError("configured crate feature record is malformed")
            features.setdefault(label, set()).add(feature)
        else:
            raise ValueError(f"license material inventory record is malformed: {line}")
    return files, features


def runfiles_manifest() -> dict[str, Path]:
    path = os.environ.get("RUNFILES_MANIFEST_FILE")
    if not path:
        return {}
    result: dict[str, Path] = {}
    try:
        with open(path, encoding="utf-8") as source:
            for line in source:
                logical, separator, physical = line.rstrip("\n").partition(" ")
                if separator:
                    result[logical] = Path(physical)
    except OSError as error:
        raise ValueError(f"runfiles manifest is unavailable: {path}") from error
    return result


def resolve_material(
    kind: str,
    logical: str,
    runfiles_root: Path | None,
    manifest: dict[str, Path],
) -> Path:
    workspace = os.environ.get("TEST_WORKSPACE", "_main")
    logical_candidates = (
        [logical]
        if kind == "external"
        else [f"{workspace}/{logical}", f"_main/{logical}"]
    )
    for key in logical_candidates:
        candidate = manifest.get(key)
        if candidate is not None and candidate.exists():
            return candidate
    if runfiles_root is not None:
        candidates = (
            [runfiles_root / logical]
            if kind == "external"
            else [
                runfiles_root / workspace / logical,
                runfiles_root / "_main" / logical,
                runfiles_root / logical,
            ]
        )
        for candidate in candidates:
            if candidate.exists():
                return candidate
    raise ValueError(f"declared release license material is unavailable: {logical}")


def inherited_package_value(
    package: dict[str, Any],
    workspace_package: dict[str, Any],
    name: str,
) -> object:
    value = package.get(name)
    if isinstance(value, dict) and value == {"workspace": True}:
        return workspace_package.get(name)
    return value


def normalize_license_expression(value: object, package: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"Cargo package {package} has no license expression")
    expression = re.sub(r"\s*/\s*", " OR ", value.strip())
    expression = re.sub(r"\s+", " ", expression)
    if re.fullmatch(r"[A-Za-z0-9.+() -]+", expression) is None:
        raise ValueError(f"Cargo package {package} has an invalid license expression")
    return expression


def package_metadata(
    selected: set[Identity],
    material_inventory_bytes: bytes,
    runfiles_root: Path | None,
) -> tuple[dict[Identity, dict[str, Any]], dict[str, set[str]], str]:
    file_records, configured_features = parse_material_inventory(
        material_inventory_bytes
    )
    manifest_map = runfiles_manifest()
    material_bytes: dict[tuple[str, str], bytes] = {}
    for kind, logical in file_records:
        material_bytes[(kind, logical)] = resolved_regular_bytes(
            resolve_material(kind, logical, runfiles_root, manifest_map),
            f"release license material {logical}",
            4 * 1024 * 1024,
        )

    root_manifest_bytes = material_bytes.get(("main", "Cargo.toml"))
    if root_manifest_bytes is None:
        raise ValueError("license materials omit the workspace Cargo.toml")
    try:
        root_manifest = tomllib.loads(root_manifest_bytes.decode())
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise ValueError("workspace Cargo.toml is malformed") from error
    workspace_package = root_manifest.get("workspace", {}).get("package", {})
    if not isinstance(workspace_package, dict):
        raise ValueError("workspace package metadata is malformed")

    manifests: dict[Identity, dict[str, Any]] = {}
    notice_files: dict[str, list[dict[str, object]]] = {}
    for (kind, logical), payload in material_bytes.items():
        path = PurePosixPath(logical)
        if path.name == "Cargo.toml":
            try:
                manifest = tomllib.loads(payload.decode())
            except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
                raise ValueError(f"Cargo manifest is malformed: {logical}") from error
            package = manifest.get("package")
            if not isinstance(package, dict):
                if logical == "Cargo.toml":
                    continue
                raise ValueError(f"Cargo manifest has no package: {logical}")
            name = inherited_package_value(package, workspace_package, "name")
            version = inherited_package_value(package, workspace_package, "version")
            if not isinstance(name, str) or not isinstance(version, str):
                raise ValueError(f"Cargo manifest identity is malformed: {logical}")
            candidates = [
                identity
                for identity in selected
                if identity[0] == name
                and identity[1] == version
                and ((kind == "main") == (identity[2] == ""))
            ]
            if len(candidates) != 1:
                if kind == "main" and not candidates:
                    continue
                raise ValueError(
                    f"Cargo manifest does not map uniquely to the target closure: {logical}"
                )
            identity = candidates[0]
            if identity in manifests:
                raise ValueError(f"duplicate Cargo manifest for {name} {version}")
            license_expression = normalize_license_expression(
                inherited_package_value(package, workspace_package, "license"),
                f"{name} {version}",
            )
            manifests[identity] = {
                "authors": inherited_package_value(
                    package, workspace_package, "authors"
                ),
                "homepage": inherited_package_value(
                    package, workspace_package, "homepage"
                ),
                "license": license_expression,
                "logical": logical,
                "repository": inherited_package_value(
                    package, workspace_package, "repository"
                ),
            }
            continue
        basename = path.name.lower()
        if not basename.startswith(NOTICE_BASENAMES):
            raise ValueError(f"unexpected license material file: {logical}")
        group = path.parts[0] if kind == "external" else "workspace"
        try:
            text = payload.decode("utf-8")
        except UnicodeDecodeError as error:
            raise ValueError(f"license material is not UTF-8 text: {logical}") from error
        notice_files.setdefault(group, []).append(
            {
                "logical": logical,
                "sha256": sha256_bytes(payload),
                "text": text,
            }
        )

    if set(manifests) != selected:
        missing = sorted(set(selected) - set(manifests))
        extra = sorted(set(manifests) - set(selected))
        raise ValueError(
            "license materials do not exactly cover the selected Cargo closure: "
            f"missing={missing[:8]} extra={extra[:8]}"
        )
    for identity, metadata in manifests.items():
        logical = PurePosixPath(str(metadata["logical"]))
        group = logical.parts[0] if identity[2] else "workspace"
        metadata["notice_files"] = sorted(
            notice_files.get(group, []),
            key=lambda item: str(item["logical"]),
        )
    return manifests, configured_features, sha256_bytes(material_inventory_bytes)


def assert_tantivy_contract(
    workspace_manifest_bytes: bytes,
    index_manifest_bytes: bytes,
    packages: list[dict[str, Any]],
    selected: set[Identity],
    configured_features: dict[str, set[str]],
) -> Identity:
    try:
        workspace_manifest = tomllib.loads(workspace_manifest_bytes.decode())
        index_manifest = tomllib.loads(index_manifest_bytes.decode())
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise ValueError("Tantivy release package manifests are malformed") from error
    declaration = workspace_manifest.get("workspace", {}).get(
        "dependencies", {}
    ).get("tantivy")
    if (
        not isinstance(declaration, dict)
        or set(declaration) != {"version", "default-features", "features"}
        or declaration.get("version") != TANTIVY_VERSION
        or declaration.get("default-features") is not False
        or sorted(declaration.get("features", [])) != TANTIVY_FEATURES
        or len(declaration.get("features", [])) != len(TANTIVY_FEATURES)
    ):
        raise ValueError(
            "workspace Tantivy must be exactly 0.26.1 with defaults off and "
            "mmap/lz4/columnar-zstd features"
        )
    index_tantivy = index_manifest.get("dependencies", {}).get("tantivy")
    if index_tantivy != {"workspace": True}:
        raise ValueError(
            "ctx-history-index must consume the exact workspace Tantivy declaration"
        )
    candidates = [
        identity
        for identity in selected
        if identity[0] == "tantivy" and identity[1] == TANTIVY_VERSION
    ]
    if len(candidates) != 1:
        raise ValueError("target closure must select exactly Tantivy 0.26.1")
    if any(identity[0] == "rust-stemmers" for identity in selected):
        raise ValueError("target closure enables Tantivy's forbidden default stemmer")
    feature_sets = [
        features
        for label, features in configured_features.items()
        if re.search(r"crates__tantivy-0\.26\.1//:tantivy$", label)
    ]
    if len(feature_sets) != 1 or sorted(feature_sets[0]) != TANTIVY_RESOLVED_FEATURES:
        raise ValueError(
            "configured Bazel Tantivy features do not match the exact defaults-off "
            "release closure"
        )
    lock_candidates = [
        package_identity(package)
        for package in packages
        if package.get("name") == "tantivy"
    ]
    if lock_candidates != [candidates[0]]:
        raise ValueError("Cargo.lock Tantivy identity is not exactly 0.26.1")
    return candidates[0]


def selected_adjacency(
    packages: list[dict[str, Any]],
    selected: set[Identity],
) -> dict[Identity, list[Identity]]:
    adjacency: dict[Identity, list[Identity]] = {}
    for package in packages:
        identity = package_identity(package)
        if identity not in selected:
            continue
        dependencies = []
        for item in package.get("dependencies", []):
            dependency = parse_dependency_identity(item, packages)
            if dependency in selected:
                dependencies.append(dependency)
        adjacency[identity] = sorted(set(dependencies))
    root = next(
        identity for identity in selected if identity[0] == "ctx" and identity[2] == ""
    )
    reachable = {root}
    pending = [root]
    while pending:
        current = pending.pop()
        for dependency in adjacency[current]:
            if dependency not in reachable:
                reachable.add(dependency)
                pending.append(dependency)
    if reachable != selected:
        missing = sorted(selected - reachable)
        raise ValueError(
            "configured Cargo inventory contains packages outside the ctx closure: "
            + ", ".join(f"{name} {version}" for name, version, _ in missing[:8])
        )
    return adjacency


def tantivy_closure(
    root: Identity,
    adjacency: dict[Identity, list[Identity]],
    packages_by_identity: dict[Identity, dict[str, Any]],
    metadata: dict[Identity, dict[str, Any]],
) -> tuple[list[dict[str, object]], str]:
    closure = {root}
    pending = [root]
    while pending:
        current = pending.pop()
        for dependency in adjacency[current]:
            if dependency not in closure:
                closure.add(dependency)
                pending.append(dependency)
    required = {"fs4", "lz4_flex", "memmap2", "tempfile", "zstd"}
    present = {identity[0] for identity in closure}
    if not required.issubset(present):
        raise ValueError(
            "configured Tantivy dependency closure omits required feature packages: "
            + ", ".join(sorted(required - present))
        )
    records = []
    for identity in sorted(closure):
        package = packages_by_identity[identity]
        record: dict[str, object] = {
            "license": metadata[identity]["license"],
            "name": identity[0],
            "source": identity[2],
            "version": identity[1],
        }
        if package.get("checksum") is not None:
            record["checksum"] = package["checksum"]
        records.append(record)
    return records, sha256_bytes(canonical(records))


def cargo_materials(
    packages: list[dict[str, Any]],
    selected: set[Identity],
    package_metadata_by_identity: dict[Identity, dict[str, Any]],
    adjacency: dict[Identity, list[Identity]],
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], str]:
    refs: dict[Identity, str] = {}
    components: list[dict[str, Any]] = []
    for package in packages:
        identity = package_identity(package)
        if identity not in selected:
            continue
        name, version, source = identity
        checksum = package.get("checksum")
        ref = material_ref("cargo", "\0".join(identity))
        refs[identity] = ref
        metadata = package_metadata_by_identity[identity]
        license_hashes = sorted(
            {str(item["sha256"]) for item in metadata["notice_files"]}
        )
        component: dict[str, Any] = {
            "type": "library",
            "bom-ref": ref,
            "name": name,
            "version": version,
            "licenses": [{"expression": metadata["license"]}],
            "purl": f"pkg:cargo/{quote(name, safe='')}@{quote(version, safe='')}",
            "properties": properties(
                {
                    "ctx:dependency:ecosystem": "cargo",
                    "ctx:dependency:source": source or "workspace",
                    "ctx:license:notice-file-sha256": license_hashes,
                }
            ),
        }
        if checksum is not None:
            component["hashes"] = [{"alg": "SHA-256", "content": checksum}]
        references = []
        for reference_type in ("homepage", "repository"):
            value = metadata.get(reference_type)
            if isinstance(value, str) and value.startswith(("https://", "http://")):
                references.append({"type": reference_type, "url": value})
        if references:
            component["externalReferences"] = references
        if name == "tantivy":
            component["properties"] = properties(
                {
                    "ctx:dependency:ecosystem": "cargo",
                    "ctx:dependency:source": source,
                    "ctx:license:notice-file-sha256": license_hashes,
                    "ctx:rust:default-features": False,
                    "ctx:rust:features": TANTIVY_FEATURES,
                    "ctx:rust:resolved-crate-features": TANTIVY_RESOLVED_FEATURES,
                }
            )
        components.append(component)

    dependencies = [
        {
            "ref": refs[identity],
            "dependsOn": sorted(refs[dependency] for dependency in adjacency[identity]),
        }
        for identity in sorted(selected)
    ]
    roots = [
        refs[identity]
        for identity in selected
        if identity[0] == "ctx" and identity[2] == ""
    ]
    if len(roots) != 1:
        raise ValueError("Cargo.lock must contain one workspace ctx package")
    return components, dependencies, roots[0]
