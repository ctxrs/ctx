#!/usr/bin/env python3
"""Create and verify deterministic Linux Core Bazel build information."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import json
import os
import re
import stat
import subprocess
import tempfile
from pathlib import Path
from typing import Any, Iterator, Sequence

try:
    import tomllib
except ModuleNotFoundError:
    try:
        import tomli as tomllib
    except ModuleNotFoundError:
        tomllib = None


MAX_ARTIFACT_SIZE = 256 * 1024 * 1024
MAX_BUILD_INFO_SIZE = 64 * 1024
MAX_INPUT_SIZE = 32 * 1024 * 1024
IMAGE_ID = re.compile(r"sha256:[0-9a-f]{64}")
LOWER_COMMIT = re.compile(r"[0-9a-f]{40}")
BAZEL_VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+")
TOML_TABLE = re.compile(
    r"^\s*\[\s*([A-Za-z0-9_.-]+)\s*\]\s*(?:#.*)?$"
)
TOML_VERSION = re.compile(
    r'^\s*version\s*=\s*"([^"\\\r\n]+)"\s*(?:#.*)?$'
)
CONTAINER_GATE_INPUTS = (
    (Path("scripts/check-public-cli-artifact.sh"), 0o555),
    (Path("scripts/check-release-binary-compat.sh"), 0o555),
    (Path("scripts/public-cli-host-runtime-evidence.sh"), 0o555),
    (Path("scripts/public-cli-runtime-authority.sh"), 0o555),
    (Path("scripts/run-native-candidate-smoke.sh"), 0o555),
    (Path("contracts/public-control-surface-v1.json"), 0o444),
    (Path("tests/fixtures/custom-history-jsonl/basic.jsonl"), 0o444),
)
AMBIENT_DOCKER_SELECTORS = (
    "DOCKER_HOST",
    "DOCKER_CONTEXT",
    "DOCKER_CONFIG",
    "DOCKER_CERT_PATH",
    "DOCKER_TLS",
    "DOCKER_TLS_VERIFY",
    "DOCKER_DEFAULT_PLATFORM",
    "DOCKER_API_VERSION",
    "BUILDX_BUILDER",
    "BUILDKIT_HOST",
)


class BuildInfoError(ValueError):
    pass


def regular_bytes(path: Path, label: str, maximum: int = MAX_INPUT_SIZE) -> bytes:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise BuildInfoError(f"{label} is unavailable: {path}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise BuildInfoError(f"{label} is not a regular non-symlink file: {path}")
    if metadata.st_size <= 0 or metadata.st_size > maximum:
        raise BuildInfoError(f"{label} has an invalid size: {path}")
    try:
        return path.read_bytes()
    except OSError as error:
        raise BuildInfoError(f"{label} could not be read: {path}") from error


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def canonical_json(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def run_checked(
    command: list[str],
    label: str,
    *,
    environment: dict[str, str] | None = None,
) -> str:
    try:
        result = subprocess.run(
            command,
            check=False,
            capture_output=True,
            env=environment,
            text=True,
        )
    except OSError as error:
        raise BuildInfoError(f"{label} could not start") from error
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        suffix = f": {detail}" if detail else ""
        raise BuildInfoError(f"{label} failed{suffix}")
    return result.stdout.rstrip("\r\n")


def git_output(source_repo: Path, *arguments: str) -> str:
    return run_checked(
        ["git", "-C", str(source_repo), *arguments],
        "Git source inspection",
    )


def validate_source(source_repo: Path, expected_commit: str) -> None:
    if LOWER_COMMIT.fullmatch(expected_commit) is None or expected_commit == "0" * 40:
        raise BuildInfoError("source commit must be nonzero lowercase 40-hex")
    if git_output(source_repo, "rev-parse", "--is-inside-work-tree") != "true":
        raise BuildInfoError("source repository is not a Git worktree")
    observed_commit = git_output(
        source_repo, "rev-parse", "--verify", "HEAD^{commit}"
    )
    if observed_commit != expected_commit:
        raise BuildInfoError(
            "source commit does not match the builder checkout: "
            f"expected {expected_commit}, got {observed_commit}"
        )
    if git_output(
        source_repo, "status", "--porcelain=v1", "--untracked-files=all"
    ):
        raise BuildInfoError("builder source checkout is dirty")


def release_version(cargo_toml: Path) -> str:
    cargo_toml_bytes = regular_bytes(cargo_toml, "ctx Cargo.toml")
    if tomllib is None:
        return release_version_without_tomllib(cargo_toml_bytes)
    try:
        value = tomllib.loads(cargo_toml_bytes.decode("utf-8"))
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise BuildInfoError("ctx Cargo.toml is malformed") from error
    version = value.get("package", {}).get("version")
    if not isinstance(version, str) or not version:
        raise BuildInfoError("ctx Cargo.toml does not declare a package version")
    return version


def release_version_without_tomllib(cargo_toml_bytes: bytes) -> str:
    """Read the one release-critical Cargo key on Python 3.10 builders."""
    try:
        source = cargo_toml_bytes.decode("utf-8")
    except UnicodeDecodeError as error:
        raise BuildInfoError("ctx Cargo.toml is malformed") from error

    in_package = False
    package_seen = False
    version: str | None = None
    for line in source.splitlines():
        table = TOML_TABLE.fullmatch(line)
        if table is not None:
            in_package = table.group(1) == "package"
            if in_package:
                if package_seen:
                    raise BuildInfoError("ctx Cargo.toml is malformed")
                package_seen = True
            continue
        if line.lstrip().startswith("["):
            in_package = False
            continue
        if not in_package or not re.match(r"^\s*version\s*=", line):
            continue
        match = TOML_VERSION.fullmatch(line)
        if match is None or version is not None:
            raise BuildInfoError("ctx Cargo.toml is malformed")
        version = match.group(1)

    if version is None:
        raise BuildInfoError("ctx Cargo.toml does not declare a package version")
    return version


def target_from_matrix(matrix_bytes: bytes, platform: str) -> dict[str, Any]:
    try:
        matrix = json.loads(matrix_bytes)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise BuildInfoError("release-target matrix is malformed") from error
    if not isinstance(matrix, dict) or matrix.get("schema_version") != 1:
        raise BuildInfoError("release-target matrix schema is invalid")
    targets = matrix.get("targets")
    if not isinstance(targets, list):
        raise BuildInfoError("release-target matrix targets are invalid")
    target_id = "linux-arm64" if platform == "linux-aarch64" else platform
    matches = [
        target
        for target in targets
        if isinstance(target, dict) and target.get("id") == target_id
    ]
    if len(matches) != 1:
        raise BuildInfoError("release-target matrix does not contain the exact target")
    target = matches[0]
    expected = {
        "linux-x64": ("x86_64", "x86_64-unknown-linux-gnu"),
        "linux-aarch64": ("aarch64", "aarch64-unknown-linux-gnu"),
    }
    expected_target = expected.get(platform)
    if (
        expected_target is None
        or target.get("os") != "linux"
        or (target.get("arch"), target.get("public_rust_target"))
        != expected_target
        or not isinstance(target.get("linux_build"), dict)
    ):
        raise BuildInfoError("producer only accepts an owned native Linux target")
    return target


def validate_rust_version(rust_version: str, linux_build: dict[str, Any]) -> None:
    toolchain = linux_build.get("rust_toolchain")
    commit = linux_build.get("rust_commit")
    if (
        not isinstance(toolchain, str)
        or not isinstance(commit, str)
        or LOWER_COMMIT.fullmatch(commit) is None
        or re.fullmatch(
            rf"rustc {re.escape(toolchain)} "
            rf"\({re.escape(commit[:9])} \d{{4}}-\d{{2}}-\d{{2}}\)",
            rust_version,
        )
        is None
    ):
        raise BuildInfoError("Bazel rustc does not match the matrix toolchain")


def artifact_observations(
    artifact: Path,
    expected_commit: str,
    cargo_lock_sha256: str,
    target_triple: str,
    version: str,
) -> tuple[bytes, str]:
    artifact_bytes = regular_bytes(
        artifact, "Bazel release artifact", MAX_ARTIFACT_SIZE
    )
    if not os.access(artifact, os.X_OK):
        raise BuildInfoError("Bazel release artifact is not executable")
    clean_environment = {"PATH": os.environ.get("PATH", "/usr/bin:/bin")}
    identity = run_checked(
        [str(artifact), "_release-build-identity"],
        "Bazel artifact identity",
        environment=clean_environment,
    )
    expected_identity = "\n".join(
        (
            f"CTX_RELEASE_BUILD_SOURCE_COMMIT={expected_commit}",
            f"CTX_RELEASE_BUILD_CARGO_LOCK_SHA256={cargo_lock_sha256}",
            f"CTX_RELEASE_BUILD_TARGET={target_triple}",
        )
    )
    if identity != expected_identity:
        raise BuildInfoError(
            "Bazel artifact identity does not match source, Cargo.lock, and target"
        )
    observed_version = run_checked(
        [str(artifact), "--version"],
        "Bazel artifact version",
        environment=clean_environment,
    )
    if observed_version != f"ctx {version}":
        raise BuildInfoError(
            f"Bazel artifact version mismatch: expected ctx {version}, "
            f"got {observed_version}"
        )
    return artifact_bytes, observed_version


def bazel_inputs(
    bazel_version_file: Path,
    module_file: Path,
    module_lock: Path,
    matrix_bytes: bytes,
    rust_version: str,
) -> dict[str, str]:
    try:
        version = regular_bytes(
            bazel_version_file, ".bazelversion", 128
        ).decode("ascii").strip()
    except UnicodeDecodeError as error:
        raise BuildInfoError(".bazelversion is not ASCII") from error
    if BAZEL_VERSION.fullmatch(version) is None:
        raise BuildInfoError(".bazelversion is malformed")
    return {
        "module_file_sha256": sha256_bytes(
            regular_bytes(module_file, "MODULE.bazel")
        ),
        "module_lock_sha256": sha256_bytes(
            regular_bytes(module_lock, "MODULE.bazel.lock")
        ),
        "release_target_matrix_sha256": sha256_bytes(matrix_bytes),
        "rustc_version": rust_version,
        "version": version,
    }


def image_id(value: object, label: str) -> str:
    if not isinstance(value, str) or IMAGE_ID.fullmatch(value) is None:
        raise BuildInfoError(f"{label} image ID is not an immutable sha256 digest")
    return value


def expected_document(
    *,
    artifact_sha256: str,
    cargo_lock_sha256: str,
    source_commit: str,
    platform: str,
    target: dict[str, Any],
    rust_version: str,
    version: str,
    builder_image_id: str,
    runtime_image_id: str,
    inspector_image_id: str,
    builder_recipe_sha256: str,
    bazel: dict[str, str],
) -> dict[str, Any]:
    linux_build = target["linux_build"]
    builder_image = linux_build["builder_image"]
    expected_base = builder_image.rsplit("@", 1)[1]
    return {
        "artifact_sha256": artifact_sha256,
        "bazel": bazel,
        "build_system": "bazel",
        "builder": {
            "base_image": {"actual": expected_base, "expected": expected_base},
            "image_id": builder_image_id,
            "recipe_sha256": builder_recipe_sha256,
        },
        "cargo_lock_sha256": cargo_lock_sha256,
        "gates": {
            "local_runtime": "passed",
            "local_runtime_authority": "authoritative",
            "static": "passed",
            "static_abi": "passed",
        },
        "inspector": {"image_id": inspector_image_id},
        "linux_build": linux_build,
        "platform": platform,
        "release_version": version,
        "representative_cpu_proof": {"profile": None, "qemu_version": None},
        "runtime": {"image_id": runtime_image_id},
        "rust_version": rust_version,
        "schema_version": 1,
        "source": {"clean": True, "commit": source_commit},
        "target": target["public_rust_target"],
    }


def docker_inspect(
    docker_command: Sequence[str], image: str, template: str, label: str
) -> str:
    return run_checked(
        [*docker_command, "image", "inspect", image, "--format", template],
        f"{label} image inspection",
    )


def verify_image(
    docker_command: Sequence[str],
    image: str,
    label: str,
    expected_labels: dict[str, str],
) -> None:
    if docker_inspect(docker_command, image, "{{.Id}}", label) != image:
        raise BuildInfoError(f"{label} image ID does not resolve exactly")
    for key, expected in expected_labels.items():
        observed = docker_inspect(
            docker_command,
            image,
            f'{{{{index .Config.Labels "{key}"}}}}',
            label,
        )
        if observed != expected:
            raise BuildInfoError(
                f"{label} image label mismatch for {key}: "
                f"expected {expected}, got {observed or 'missing'}"
            )


@contextmanager
def staged_container_gate_source(source_repo: Path) -> Iterator[Path]:
    """Expose only reviewed gate inputs to the unprivileged containers."""
    with tempfile.TemporaryDirectory(prefix="ctx-public-container-gates-") as temporary:
        gate_root = Path(temporary)
        for relative_path, mode in CONTAINER_GATE_INPUTS:
            destination = gate_root / relative_path
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(
                regular_bytes(
                    source_repo / relative_path,
                    f"container gate input {relative_path}",
                )
            )
            destination.chmod(mode)
        gate_root.chmod(0o755)
        for directory in (path for path in gate_root.rglob("*") if path.is_dir()):
            directory.chmod(0o755)
        yield gate_root


def run_container_gates(
    *,
    docker_command: Sequence[str],
    source_repo: Path,
    artifact: Path,
    version: str,
    platform: str,
    builder_image_id: str,
    runtime_image_id: str,
    inspector_image_id: str,
) -> None:
    artifact_dir = artifact.parent
    docker_platform = {
        "linux-x64": "linux/amd64",
        "linux-aarch64": "linux/arm64",
    }.get(platform)
    if docker_platform is None:
        raise BuildInfoError("container gates require an owned native Linux target")
    common = [
        *docker_command,
        "run",
        "--rm",
        "--platform",
        docker_platform,
        "--network",
        "none",
        "--user",
        "65534:65534",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--read-only",
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,exec",
    ]
    with staged_container_gate_source(source_repo) as gate_source:
        authority = run_checked(
            common
            + [
                "-v",
                f"{gate_source}:/repo:ro",
                "-w",
                "/repo",
                builder_image_id,
                "bash",
                "-euo",
                "pipefail",
                "-c",
                (
                    "IFS=$'\\t' read -r host_system host_arch "
                    "host_native_arch process_translated _native_arch_probe "
                    "hardware_identity emulation hypervisor evidence_complete "
                    "< <(scripts/public-cli-host-runtime-evidence.sh); "
                    "scripts/public-cli-runtime-authority.sh "
                    "\"$1\" \"$host_system\" \"$host_arch\" passed "
                    "\"$host_native_arch\" \"$process_translated\" "
                    "\"$hardware_identity\" \"$emulation\" "
                    "\"$hypervisor\" \"$evidence_complete\""
                ),
                "bash",
                platform,
            ],
            "pinned builder authority gate",
        )
        if authority != "authoritative":
            raise BuildInfoError(
                "pinned builder authority gate returned "
                f"{authority or 'no authority'}"
            )
        run_checked(
            common
            + [
                "-e",
                f"CTX_PUBLIC_CLI_EXPECTED_VERSION={version}",
                "-v",
                f"{gate_source}:/repo:ro",
                "-v",
                f"{artifact_dir}:/artifacts:ro",
                "-w",
                "/repo",
                inspector_image_id,
                "bash",
                "scripts/check-public-cli-artifact.sh",
                platform,
                "/artifacts",
            ],
            "pinned inspector static ABI gate",
        )
        run_checked(
            common
            + [
                "-e",
                "HOME=/tmp/home",
                "-v",
                f"{gate_source}:/repo:ro",
                "-v",
                f"{artifact}:/candidate/ctx:ro",
                "-w",
                "/repo",
                runtime_image_id,
                "bash",
                "-euo",
                "pipefail",
                "-c",
                (
                    "install -d -m 0700 /tmp/candidate && "
                    "install -m 0755 /candidate/ctx /tmp/candidate/ctx && "
                    "timeout --signal=KILL 120s "
                    "bash scripts/run-native-candidate-smoke.sh "
                    "/tmp/candidate/ctx "
                    "tests/fixtures/custom-history-jsonl/basic.jsonl "
                    '"$1" /tmp/native-smoke.json '
                    "&& grep -Fq '\"status\":\"passed\"' /tmp/native-smoke.json"
                ),
                "bash",
                version,
            ],
            "pinned native runtime gate",
        )


def validate_inputs(
    args: argparse.Namespace, rust_version: str
) -> tuple[dict[str, Any], bytes, bytes, dict[str, str], str]:
    source_repo = args.source_repo.resolve(strict=True)
    validate_source(source_repo, args.source_commit)
    observed_version = release_version(args.cargo_toml)
    if observed_version != args.version:
        raise BuildInfoError(
            f"source version mismatch: expected {args.version}, got {observed_version}"
        )
    matrix_bytes = regular_bytes(args.matrix, "release-target matrix")
    target = target_from_matrix(matrix_bytes, args.platform)
    linux_build = target["linux_build"]
    validate_rust_version(rust_version, linux_build)
    cargo_lock_bytes = regular_bytes(args.cargo_lock, "Cargo.lock")
    cargo_lock_sha256 = sha256_bytes(cargo_lock_bytes)
    artifact_bytes, _ = artifact_observations(
        args.artifact,
        args.source_commit,
        cargo_lock_sha256,
        target["public_rust_target"],
        args.version,
    )
    bazel = bazel_inputs(
        args.bazel_version_file,
        args.module_file,
        args.module_lock,
        matrix_bytes,
        rust_version,
    )
    recipe_sha256 = sha256_bytes(
        regular_bytes(args.builder_recipe, "Linux Bazel builder recipe")
    )
    return target, artifact_bytes, cargo_lock_bytes, bazel, recipe_sha256


def add_common_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--bazel-version-file", type=Path, required=True)
    parser.add_argument("--builder-recipe", type=Path, required=True)
    parser.add_argument("--cargo-lock", type=Path, required=True)
    parser.add_argument("--cargo-toml", type=Path, required=True)
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--module-file", type=Path, required=True)
    parser.add_argument("--module-lock", type=Path, required=True)
    parser.add_argument(
        "--platform",
        choices=("linux-aarch64", "linux-x64"),
        required=True,
    )
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--source-repo", type=Path, required=True)
    parser.add_argument("--version", required=True)


def create(args: argparse.Namespace) -> None:
    rust_version = args.rust_version.strip()
    target, artifact_bytes, cargo_lock_bytes, bazel, recipe_sha256 = (
        validate_inputs(args, rust_version)
    )
    builder_image_id = image_id(args.builder_image_id, "builder")
    runtime_image_id = image_id(args.runtime_image_id, "runtime")
    inspector_image_id = image_id(args.inspector_image_id, "inspector")
    linux_build = target["linux_build"]
    base_labels = {
        "org.ctx.release.arch": target["arch"],
        "org.ctx.release.base-image": linux_build["builder_image"],
        "org.ctx.release.ubuntu-snapshot": linux_build["ubuntu_snapshot"],
    }
    docker_command = [
        "/usr/bin/env",
        *(
            value
            for selector in AMBIENT_DOCKER_SELECTORS
            for value in ("-u", selector)
        ),
        f"HOME={args.docker_home}",
        args.docker,
        "--host",
        args.docker_host,
        "--config",
        str(args.docker_config),
    ]
    verify_image(
        docker_command,
        builder_image_id,
        "builder",
        {
            **base_labels,
            "org.ctx.release.bazel-version": bazel["version"],
            "org.ctx.release.glibc-baseline": linux_build["glibc_max"],
            "org.ctx.release.role": "ctx-public-bazel-builder",
            "org.ctx.release.rust-commit": linux_build["rust_commit"],
            "org.ctx.release.rust-toolchain": linux_build["rust_toolchain"],
        },
    )
    verify_image(
        docker_command,
        runtime_image_id,
        "runtime",
        {**base_labels, "org.ctx.release.role": "runtime"},
    )
    verify_image(
        docker_command,
        inspector_image_id,
        "inspector",
        {**base_labels, "org.ctx.release.role": "inspector"},
    )
    run_container_gates(
        docker_command=docker_command,
        source_repo=args.source_repo.resolve(strict=True),
        artifact=args.artifact.resolve(strict=True),
        version=args.version,
        platform=args.platform,
        builder_image_id=builder_image_id,
        runtime_image_id=runtime_image_id,
        inspector_image_id=inspector_image_id,
    )
    validate_source(args.source_repo.resolve(strict=True), args.source_commit)
    current_artifact = regular_bytes(
        args.artifact, "Bazel release artifact", MAX_ARTIFACT_SIZE
    )
    if current_artifact != artifact_bytes:
        raise BuildInfoError("Bazel release artifact changed during builder gates")
    if regular_bytes(args.cargo_lock, "Cargo.lock") != cargo_lock_bytes:
        raise BuildInfoError("Cargo.lock changed during builder gates")
    document = expected_document(
        artifact_sha256=sha256_bytes(artifact_bytes),
        cargo_lock_sha256=sha256_bytes(cargo_lock_bytes),
        source_commit=args.source_commit,
        platform=args.platform,
        target=target,
        rust_version=rust_version,
        version=args.version,
        builder_image_id=builder_image_id,
        runtime_image_id=runtime_image_id,
        inspector_image_id=inspector_image_id,
        builder_recipe_sha256=recipe_sha256,
        bazel=bazel,
    )
    output = args.output
    if output.exists() or output.is_symlink():
        raise BuildInfoError(f"build-info output already exists: {output}")
    output.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    descriptor = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "wb") as destination:
        destination.write(canonical_json(document))
        destination.flush()
        os.fsync(destination.fileno())


def verify(args: argparse.Namespace) -> str:
    rust_version = run_checked(
        [str(args.rustc), "--version"],
        "declared Bazel rustc inspection",
    )
    target, artifact_bytes, cargo_lock_bytes, bazel, recipe_sha256 = (
        validate_inputs(args, rust_version)
    )
    build_info_bytes = regular_bytes(
        args.build_info, "Bazel build-info", MAX_BUILD_INFO_SIZE
    )
    try:
        observed = json.loads(build_info_bytes)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise BuildInfoError("Bazel build-info is malformed") from error
    if not isinstance(observed, dict):
        raise BuildInfoError("Bazel build-info is malformed")
    builder = observed.get("builder")
    runtime = observed.get("runtime")
    inspector = observed.get("inspector")
    builder_image_id = image_id(
        builder.get("image_id") if isinstance(builder, dict) else None,
        "builder",
    )
    runtime_image_id = image_id(
        runtime.get("image_id") if isinstance(runtime, dict) else None,
        "runtime",
    )
    inspector_image_id = image_id(
        inspector.get("image_id") if isinstance(inspector, dict) else None,
        "inspector",
    )
    expected = expected_document(
        artifact_sha256=sha256_bytes(artifact_bytes),
        cargo_lock_sha256=sha256_bytes(cargo_lock_bytes),
        source_commit=args.source_commit,
        platform=args.platform,
        target=target,
        rust_version=rust_version,
        version=args.version,
        builder_image_id=builder_image_id,
        runtime_image_id=runtime_image_id,
        inspector_image_id=inspector_image_id,
        builder_recipe_sha256=recipe_sha256,
        bazel=bazel,
    )
    expected_bytes = canonical_json(expected)
    if observed != expected or build_info_bytes != expected_bytes:
        raise BuildInfoError(
            "Bazel build-info does not match the exact source, version, "
            "target, toolchain, and builder inputs"
        )
    validate_source(args.source_repo.resolve(strict=True), args.source_commit)
    return sha256_bytes(build_info_bytes)


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    subparsers = root.add_subparsers(dest="command", required=True)
    version_parser = subparsers.add_parser("cargo-version")
    version_parser.add_argument("--cargo-toml", type=Path, required=True)
    create_parser = subparsers.add_parser("create")
    add_common_arguments(create_parser)
    create_parser.add_argument("--builder-image-id", required=True)
    create_parser.add_argument("--docker", default="docker")
    create_parser.add_argument("--docker-config", type=Path, required=True)
    create_parser.add_argument("--docker-home", type=Path, required=True)
    create_parser.add_argument("--docker-host", required=True)
    create_parser.add_argument("--inspector-image-id", required=True)
    create_parser.add_argument("--output", type=Path, required=True)
    create_parser.add_argument("--runtime-image-id", required=True)
    create_parser.add_argument("--rust-version", required=True)
    verify_parser = subparsers.add_parser("verify")
    add_common_arguments(verify_parser)
    verify_parser.add_argument("--build-info", type=Path, required=True)
    verify_parser.add_argument("--rustc", type=Path, required=True)
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        if args.command == "cargo-version":
            print(release_version(args.cargo_toml))
        elif args.command == "create":
            create(args)
        else:
            print(verify(args))
    except (BuildInfoError, OSError) as error:
        raise SystemExit(f"error: {error}") from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
