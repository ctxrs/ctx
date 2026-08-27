#!/usr/bin/env python3
"""Enforce a physical 21,000-CLOC limit for Cargo workspace packages."""

from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tomli as tomllib
from typing import Any, Iterable


HARD_LIMIT = 21_000
METRIC = "physical-rust-cloc-v1"
COMMIT = re.compile(r"^[0-9a-f]{40}$")
EXCLUDED_DIRECTORY_NAMES = {
    ".git",
    ".hg",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".svn",
    "__pycache__",
    "node_modules",
    "target",
}
class GateError(RuntimeError):
    pass


@dataclass(frozen=True)
class Package:
    name: str
    manifest: str
    root: str


@dataclass(frozen=True)
class Measurement:
    package: Package
    cloc: int
    files: int


def normalized_relative_path(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise GateError(f"{label} must be a nonempty relative path")
    if any(character in value for character in ("\0", "\t", "\n", "\r", "\\")):
        raise GateError(f"{label} is not normalized: {value!r}")
    if any(character in value for character in "*?["):
        raise GateError(f"{label} may not contain a glob: {value}")
    path = PurePosixPath(value)
    if path.is_absolute() or value.endswith("/") or any(part in {"", ".", ".."} for part in path.parts):
        raise GateError(f"{label} is not normalized: {value}")
    return value


def read_toml(path: Path, label: str) -> dict[str, Any]:
    try:
        value = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise GateError(f"{label} is not valid UTF-8 TOML: {error}") from error
    if not isinstance(value, dict):
        raise GateError(f"{label} root must be a table")
    return value


def has_symlink_component(root: Path, relative: str) -> bool:
    current = root
    for part in PurePosixPath(relative).parts:
        current = current / part
        if current.is_symlink():
            return True
    return False


def workspace_packages(root: Path) -> list[Package]:
    manifest = root / "Cargo.toml"
    if not manifest.is_file() or manifest.is_symlink():
        raise GateError("root Cargo.toml must be a regular non-symlink file")
    workspace = read_toml(manifest, "root Cargo.toml").get("workspace")
    if not isinstance(workspace, dict) or not isinstance(workspace.get("members"), list):
        raise GateError("root Cargo.toml must declare workspace.members")
    members = workspace["members"]
    if not members:
        raise GateError("workspace.members must not be empty")

    packages: list[Package] = []
    seen_roots: set[str] = set()
    seen_names: set[str] = set()
    for raw_member in members:
        member = normalized_relative_path(raw_member, "workspace member")
        if member in seen_roots:
            raise GateError(f"duplicate workspace member: {member}")
        if has_symlink_component(root, member):
            raise GateError(f"workspace package root contains a symlink component: {member}")
        package_root = root / member
        package_manifest = package_root / "Cargo.toml"
        if not package_root.is_dir() or not package_manifest.is_file() or package_manifest.is_symlink():
            raise GateError(f"workspace member has no regular Cargo.toml: {member}")
        package_table = read_toml(package_manifest, f"{member}/Cargo.toml").get("package")
        name = package_table.get("name") if isinstance(package_table, dict) else None
        if not isinstance(name, str) or not name:
            raise GateError(f"workspace member package.name is malformed: {member}")
        if name in seen_names:
            raise GateError(f"duplicate workspace package name: {name}")
        seen_roots.add(member)
        seen_names.add(name)
        packages.append(Package(name=name, manifest=f"{member}/Cargo.toml", root=member))

    roots = sorted(seen_roots)
    for index, package_root in enumerate(roots):
        nested = [other for other in roots[index + 1 :] if other.startswith(package_root + "/")]
        if nested:
            raise GateError(f"overlapping or nested workspace package roots: {package_root}, {nested[0]}")
    return sorted(packages, key=lambda package: package.name)


def checkout_artifact_directory(relative: PurePosixPath) -> bool:
    name = relative.name
    if name in EXCLUDED_DIRECTORY_NAMES:
        return True
    return len(relative.parts) == 1 and (name.startswith("bazel-") or name == ".buildkite-cache")


def beneath_package(relative: PurePosixPath, package_roots: tuple[str, ...]) -> bool:
    path = relative.as_posix()
    return any(path == package_root or path.startswith(package_root + "/") for package_root in package_roots)


def physical_repository_files(
    root: Path, packages: list[Package]
) -> tuple[set[str], set[str]]:
    rust_files: set[str] = set()
    manifests: set[str] = set()
    package_roots = tuple(package.root for package in packages)

    def visit(directory: Path, relative: PurePosixPath) -> None:
        try:
            entries = sorted(os.scandir(directory), key=lambda entry: entry.name)
        except OSError as error:
            raise GateError(f"cannot scan repository directory {relative.as_posix()}: {error}") from error
        for entry in entries:
            child_relative = relative / entry.name
            child_path = Path(entry.path)
            package_owned = beneath_package(child_relative, package_roots)
            if entry.is_symlink():
                if entry.name.endswith(".rs"):
                    raise GateError(f"symlinked Rust file is forbidden: {child_relative.as_posix()}")
                if entry.name == "Cargo.toml":
                    raise GateError(f"symlinked Cargo.toml is forbidden: {child_relative.as_posix()}")
                if package_owned and entry.is_dir(follow_symlinks=True):
                    raise GateError(
                        f"symlinked package directory is forbidden: {child_relative.as_posix()}"
                    )
                if (
                    not package_owned
                    and entry.is_dir(follow_symlinks=True)
                    and not checkout_artifact_directory(child_relative)
                ):
                    raise GateError(
                        f"symlinked repository directory is ambiguous: {child_relative.as_posix()}"
                    )
                continue
            if entry.is_dir(follow_symlinks=False):
                if package_owned or not checkout_artifact_directory(child_relative):
                    visit(child_path, child_relative)
                continue
            if entry.name.endswith(".rs"):
                if not entry.is_file(follow_symlinks=False):
                    raise GateError(f"Rust path is not a regular file: {child_relative.as_posix()}")
                rust_files.add(child_relative.as_posix())
            if entry.name == "Cargo.toml":
                if not entry.is_file(follow_symlinks=False):
                    raise GateError(f"Cargo.toml is not a regular file: {child_relative.as_posix()}")
                manifests.add(child_relative.as_posix())

    visit(root, PurePosixPath())
    return rust_files, manifests


def assign_physical_sources(
    packages: list[Package], rust_files: Iterable[str], manifests: set[str]
) -> dict[str, list[str]]:
    expected_manifests = {"Cargo.toml", *(package.manifest for package in packages)}
    if manifests != expected_manifests:
        raise GateError(
            "undeclared Cargo.toml detected: "
            f"extra={sorted(manifests-expected_manifests)}, missing={sorted(expected_manifests-manifests)}"
        )
    result = {package.name: [] for package in packages}
    for path in sorted(rust_files):
        owners = [package for package in packages if path.startswith(package.root + "/")]
        if len(owners) != 1:
            raise GateError(
                f"Rust file must belong physically to exactly one workspace package: {path}; "
                f"owners={[package.name for package in owners]}"
            )
        result[owners[0].name].append(path)
    for package in packages:
        if not result[package.name]:
            raise GateError(f"workspace package has no physical Rust files: {package.name}")
    return result


def raw_string_start(line: str, index: int) -> tuple[int, int] | None:
    if index and (line[index - 1].isalnum() or line[index - 1] == "_"):
        return None
    cursor = index
    if line.startswith(("br", "cr"), index):
        cursor += 2
    elif line.startswith("r", index):
        cursor += 1
    else:
        return None
    hashes = 0
    while cursor < len(line) and line[cursor] == "#":
        hashes += 1
        cursor += 1
    if cursor < len(line) and line[cursor] == '"':
        return hashes, cursor + 1
    return None


def rust_character_end(line: str, index: int) -> int | None:
    """Return the byte-character/character literal end, not a lifetime tick."""
    cursor = index + 1
    if cursor >= len(line) or line[cursor] in "\r\n'":
        return None
    if line[cursor] == "\\":
        cursor += 1
        if cursor >= len(line) or line[cursor] in "\r\n":
            return None
        if line[cursor] == "x":
            cursor += 3
        elif line[cursor] == "u" and cursor + 1 < len(line) and line[cursor + 1] == "{":
            closing = line.find("}", cursor + 2)
            if closing < 0:
                return None
            cursor = closing + 1
        else:
            cursor += 1
    else:
        cursor += 1
    if cursor < len(line) and line[cursor] == "'":
        return cursor + 1
    return None


def rust_cloc(content: bytes, path: str = "Rust source") -> int:
    try:
        text = content.decode("utf-8")
    except UnicodeDecodeError as error:
        raise GateError(f"{path} is not UTF-8") from error
    block_depth = 0
    string_kind: str | None = None
    raw_hashes = 0
    escaped = False
    count = 0
    for line in text.splitlines(keepends=True):
        code = string_kind is not None
        index = 0
        while index < len(line):
            if block_depth:
                if line.startswith("/*", index):
                    block_depth += 1
                    index += 2
                elif line.startswith("*/", index):
                    block_depth -= 1
                    index += 2
                else:
                    index += 1
                continue
            if string_kind == "raw":
                code = True
                terminator = '"' + ("#" * raw_hashes)
                if line.startswith(terminator, index):
                    string_kind = None
                    index += len(terminator)
                else:
                    index += 1
                continue
            if string_kind == "quoted":
                code = True
                character = line[index]
                if escaped:
                    escaped = False
                elif character == "\\":
                    escaped = True
                elif character == '"':
                    string_kind = None
                index += 1
                continue

            if line[index].isspace():
                index += 1
                continue
            if line.startswith("//", index):
                break
            if line.startswith("/*", index):
                block_depth = 1
                index += 2
                continue
            raw = raw_string_start(line, index)
            if raw is not None:
                raw_hashes, index = raw
                string_kind = "raw"
                code = True
                continue
            if line.startswith(('b"', 'c"'), index):
                string_kind = "quoted"
                escaped = False
                code = True
                index += 2
                continue
            character_index = index + 1 if line.startswith("b'", index) else index
            if line[character_index] == "'":
                character_end = rust_character_end(line, character_index)
                if character_end is not None:
                    code = True
                    index = character_end
                    continue
            if line[index] == '"':
                string_kind = "quoted"
                escaped = False
                code = True
                index += 1
                continue
            code = True
            index += 1
        if code:
            count += 1
    if block_depth:
        raise GateError(f"{path} has an unterminated block comment")
    if string_kind is not None:
        raise GateError(f"{path} has an unterminated string literal")
    return count


def measure_packages(root: Path, packages: list[Package], sources: dict[str, list[str]]) -> list[Measurement]:
    result: list[Measurement] = []
    for package in packages:
        paths = sources[package.name]
        cloc = 0
        for path in paths:
            try:
                content = (root / path).read_bytes()
            except OSError as error:
                raise GateError(f"cannot read Rust source {path}: {error}") from error
            cloc += rust_cloc(content, path)
        result.append(Measurement(package=package, cloc=cloc, files=len(paths)))
    return result


def live_measurements(root: Path) -> list[Measurement]:
    packages = workspace_packages(root)
    rust_files, manifests = physical_repository_files(root, packages)
    sources = assign_physical_sources(packages, rust_files, manifests)
    return measure_packages(root, packages, sources)


def isolated_git(root: Path, *arguments: str) -> subprocess.CompletedProcess[bytes]:
    environment = {
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_OPTIONAL_LOCKS": "0",
        "LC_ALL": "C",
        "PATH": os.environ.get("PATH", os.defpath),
    }
    result = subprocess.run(
        ["git", "-c", f"core.excludesFile={os.devnull}", *arguments],
        cwd=root,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        raise GateError(
            f"git {' '.join(arguments)} failed: {result.stderr.decode('utf-8', 'replace').strip()}"
        )
    return result


def verify_exact_candidate(root: Path, exact_candidate_commit: str) -> None:
    top = Path(isolated_git(root, "rev-parse", "--show-toplevel").stdout.decode().strip()).resolve()
    if top != root.resolve():
        raise GateError(f"exact-candidate validation requires the Git checkout root: {root}")
    if (
        COMMIT.fullmatch(exact_candidate_commit) is None
        or exact_candidate_commit == "0" * 40
    ):
        raise GateError("exact candidate commit must be nonzero lowercase 40-hex")
    head = isolated_git(root, "rev-parse", "HEAD").stdout.decode().strip()
    if head != exact_candidate_commit:
        raise GateError(
            "exact candidate commit does not match checked-out HEAD: "
            f"candidate={exact_candidate_commit} head={head}"
        )
    status = isolated_git(
        root, "status", "--porcelain=v1", "--untracked-files=all"
    ).stdout
    if status:
        raise GateError("exact candidate checkout is dirty")


def measurement_failures(measurements: list[Measurement]) -> list[str]:
    return [
        f"package={item.package.name} count={item.cloc} limit={HARD_LIMIT}"
        for item in sorted(measurements, key=lambda measured: measured.package.name)
        if item.cloc > HARD_LIMIT
    ]


def format_failures(failures: list[str]) -> str:
    return "physical Rust crate-size gate failed:\n  " + "\n  ".join(failures)


def check_checkout(
    root: Path, exact_candidate_commit: str | None = None
) -> list[Measurement]:
    if exact_candidate_commit is not None:
        verify_exact_candidate(root, exact_candidate_commit)
    measurements = live_measurements(root)
    failures = measurement_failures(measurements)
    if failures:
        raise GateError(format_failures(failures))
    return measurements


def resolve_root(value: str) -> Path:
    root = Path(value)
    if not root.is_absolute():
        raise GateError("checkout root must be absolute")
    root = root.resolve()
    if not (root / "Cargo.toml").is_file():
        raise GateError(f"checkout root has no Cargo.toml: {root}")
    return root


def main() -> int:
    usage = (
        "usage: check-rust-crate-size.py --preflight ABSOLUTE_ROOT\n"
        "       check-rust-crate-size.py --exact-candidate COMMIT ABSOLUTE_ROOT"
    )
    exact_candidate_commit = None
    if len(sys.argv) == 3 and sys.argv[1] == "--preflight":
        root = resolve_root(sys.argv[2])
    elif len(sys.argv) == 4 and sys.argv[1] == "--exact-candidate":
        exact_candidate_commit = sys.argv[2]
        root = resolve_root(sys.argv[3])
    else:
        raise GateError(usage)
    measurements = check_checkout(root, exact_candidate_commit)
    total_files = sum(item.files for item in measurements)
    total_cloc = sum(item.cloc for item in measurements)
    print(
        f"physical Rust crate-size gate passed: packages={len(measurements)} files={total_files} "
        f"cloc={total_cloc} limit={HARD_LIMIT} metric={METRIC}"
    )
    for item in measurements:
        print(f"  {item.package.name}: files={item.files} cloc={item.cloc} limit={HARD_LIMIT}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1) from None
