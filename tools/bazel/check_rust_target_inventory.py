#!/usr/bin/env python3
"""Validate live Cargo/Bazel ownership without a checked-in package inventory."""

from __future__ import annotations

import ast
from collections import defaultdict
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tomllib
from typing import Any

DEPENDENCY_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")
TARGET_KINDS = ("bin", "test", "example", "bench")
# These src-based macros forward to ctx_rust_test with its default harness:
# tools/bazel/binary_contracts.bzl and the owning crates' test_targets.bzl.
LIBTEST_CONTRACT_RULES = {
    "ctx_binary_contract_test",
    "ctx_cli_contract_test",
    "ctx_cli_integration_test",
    "agent_application_binary_contract",
    "daemon_cli_binary_contract",
    "history_ingest_binary_contract",
    "history_read_binary_contract",
    "observability_binary_contract",
}
VISIBILITY_RESTRICTED_LOCAL_LABELS = {
    "ctx-history-jsonl": {
        "//crates/ctx-history-jsonl:lib",
        "//crates/ctx-history-jsonl:test_support_lib",
    },
    "ctx-history-source-sqlite": {
        "//crates/ctx-history-source-sqlite:lib",
        "//crates/ctx-history-source-sqlite:test_support_lib",
    },
}


class InventoryError(RuntimeError):
    pass


def fail(message: str) -> None:
    raise InventoryError(message)


def git(candidate: Path, *arguments: str) -> bytes:
    result = subprocess.run(
        ["git", "-C", os.fspath(candidate), *arguments],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        env={**os.environ, "GIT_OPTIONAL_LOCKS": "0", "LC_ALL": "C"},
    )
    if result.returncode != 0:
        fail(result.stderr.decode("utf-8", "replace").strip())
    return result.stdout


def repository_root() -> Path:
    for candidate in (Path.cwd(), Path(__file__).resolve().parents[2]):
        try:
            root = Path(git(candidate, "rev-parse", "--show-toplevel").decode().strip())
        except (InventoryError, UnicodeError):
            continue
        marker = root / ".git"
        if marker.is_symlink():
            resolved = marker.resolve()
            if resolved.name == ".git":
                root = resolved.parent
        root = root.resolve()
        try:
            verified = Path(
                git(root, "rev-parse", "--show-toplevel").decode().strip()
            ).resolve()
        except (InventoryError, UnicodeError):
            continue
        if verified == root:
            return root
    fail("could not locate the physical Git worktree")


def load_toml(path: Path) -> dict[str, Any]:
    try:
        return tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        fail(f"cannot parse {path}: {error}")


def normalized_member(value: Any) -> str:
    if not isinstance(value, str) or not value:
        fail("workspace members must be nonempty strings")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
        fail(f"workspace member is not normalized: {value!r}")
    if any(character in value for character in "*?[]\\"):
        fail(f"workspace member globs are unsupported; use an exact package path: {value}")
    return path.as_posix()


def live_package_manifests(root: Path) -> set[Path]:
    result: set[Path] = set()
    raw = git(
        root,
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
        "--",
        ":(glob)**/Cargo.toml",
    )
    for item in raw.split(b"\0"):
        if not item:
            continue
        try:
            relative = Path(item.decode("utf-8"))
        except UnicodeDecodeError as error:
            raise InventoryError("Cargo manifest paths must be UTF-8") from error
        if relative == Path("Cargo.toml"):
            continue
        data = load_toml(root / relative)
        if "package" in data:
            result.add(relative)
    return result


def workspace_packages(root: Path) -> dict[str, tuple[Path, dict[str, Any]]]:
    workspace = load_toml(root / "Cargo.toml").get("workspace")
    if not isinstance(workspace, dict) or not isinstance(workspace.get("members"), list):
        fail("root Cargo.toml must define workspace.members")
    declared = {
        Path(member) / "Cargo.toml"
        for member in map(normalized_member, workspace["members"])
    }
    discovered = live_package_manifests(root)
    if declared != discovered:
        missing = sorted(path.as_posix() for path in discovered - declared)
        stale = sorted(path.as_posix() for path in declared - discovered)
        fail(f"workspace membership mismatch: missing={missing} stale={stale}")

    packages: dict[str, tuple[Path, dict[str, Any]]] = {}
    for relative in sorted(declared):
        manifest = root / relative
        if not manifest.is_file():
            fail(f"workspace manifest is missing: {relative}")
        data = load_toml(manifest)
        package = data.get("package")
        if not isinstance(package, dict) or not isinstance(package.get("name"), str):
            fail(f"workspace manifest has no package.name: {relative}")
        name = package["name"]
        if name in packages:
            fail(f"duplicate workspace package name: {name}")
        packages[name] = (manifest.parent, data)
    return packages


def explicit_targets(data: dict[str, Any], kind: str) -> list[dict[str, Any]]:
    value = data.get(kind, [])
    if not isinstance(value, list) or any(not isinstance(item, dict) for item in value):
        fail(f"Cargo [[{kind}]] targets must be tables")
    return value


def cargo_targets(package_dir: Path, data: dict[str, Any]) -> dict[str, Path]:
    package = data["package"]
    package_name = package["name"]
    targets: dict[str, Path] = {}
    lib = data.get("lib")
    if lib is not None and not isinstance(lib, dict):
        fail(f"{package_name} [lib] must be a table")
    if lib is not None or (package_dir / "src/lib.rs").is_file():
        target = lib or {}
        targets[f"lib:{target.get('name', package_name.replace('-', '_'))}"] = Path(
            target.get("path", "src/lib.rs")
        )

    defaults = {
        "bin": ("autobins", "src/main.rs", "src/bin", package_name),
        "test": ("autotests", None, "tests", None),
        "example": ("autoexamples", None, "examples", None),
        "bench": ("autobenches", None, "benches", None),
    }
    for kind in TARGET_KINDS:
        explicit = explicit_targets(data, kind)
        for item in explicit:
            name = item.get("name")
            if not isinstance(name, str) or not name:
                fail(f"{package_name} [[{kind}]] target has no name")
            default_path = f"{defaults[kind][2]}/{name}.rs"
            targets[f"{kind}:{name}"] = Path(item.get("path", default_path))
        if package.get(defaults[kind][0]) is False:
            continue
        flag, primary, directory, primary_name = defaults[kind]
        if primary and (package_dir / primary).is_file():
            targets.setdefault(f"{kind}:{primary_name}", Path(primary))
        target_dir = package_dir / directory
        if target_dir.is_dir():
            for path in sorted(target_dir.glob("*.rs")):
                targets.setdefault(f"{kind}:{path.stem}", path.relative_to(package_dir))
            for path in sorted(target_dir.glob("*/main.rs")):
                targets.setdefault(
                    f"{kind}:{path.parent.name}", path.relative_to(package_dir)
                )

    build = package.get("build")
    if build is not False and (build or (package_dir / "build.rs").is_file()):
        targets["custom-build:build-script-build"] = Path(
            build if isinstance(build, str) else "build.rs"
        )
    return targets


def package_bazel_modules(root: Path, package_dir: Path) -> list[ast.Module]:
    path = package_dir / "BUILD.bazel"
    if not path.is_file():
        fail(f"Cargo package has no BUILD.bazel: {package_dir.relative_to(root)}")
    try:
        return [ast.parse(path.read_text(encoding="utf-8"), path.as_posix())]
    except (OSError, UnicodeError, SyntaxError) as error:
        fail(f"cannot parse Bazel metadata {path.relative_to(root)}: {error}")


def assignments(module: ast.Module) -> dict[str, ast.AST]:
    return {
        node.targets[0].id: node.value
        for node in module.body
        if isinstance(node, ast.Assign)
        and len(node.targets) == 1
        and isinstance(node.targets[0], ast.Name)
    }


def string_values(
    node: ast.AST | None,
    environment: dict[str, ast.AST],
    visiting: frozenset[str] = frozenset(),
) -> set[str]:
    if node is None:
        return set()
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return {node.value}
    if isinstance(node, (ast.List, ast.Tuple, ast.Set)):
        return set().union(
            *(string_values(item, environment, visiting) for item in node.elts)
        ) if node.elts else set()
    if isinstance(node, ast.Dict):
        return set().union(
            *(string_values(item, environment, visiting) for item in node.values)
        ) if node.values else set()
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
        return string_values(node.left, environment, visiting) | string_values(
            node.right, environment, visiting
        )
    if isinstance(node, ast.Name) and node.id in environment and node.id not in visiting:
        return string_values(
            environment[node.id], environment, visiting | frozenset({node.id})
        )
    if isinstance(node, ast.Call):
        return set().union(
            *(string_values(argument, environment, visiting) for argument in node.args),
            *(string_values(keyword.value, environment, visiting) for keyword in node.keywords),
        )
    return set()


def bazel_glob_matches(pattern: str, path: str) -> bool:
    marker = "\0DOUBLESTAR_SLASH\0"
    expression = re.escape(pattern).replace(r"\*\*/", marker)
    expression = expression.replace(r"\*\*", ".*").replace(r"\*", "[^/]*")
    expression = expression.replace(r"\?", "[^/]").replace(
        re.escape(marker), "(?:[^/]+/)*"
    )
    return re.fullmatch(expression, path) is not None


def expression_owns_path(
    node: ast.AST | None,
    path: str,
    environment: dict[str, ast.AST],
    visiting: frozenset[str] = frozenset(),
) -> bool:
    if node is None:
        return False
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value == path
    if isinstance(node, (ast.List, ast.Tuple, ast.Set)):
        return any(expression_owns_path(item, path, environment, visiting) for item in node.elts)
    if isinstance(node, ast.Dict):
        return any(expression_owns_path(item, path, environment, visiting) for item in node.values)
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
        return expression_owns_path(node.left, path, environment, visiting) or expression_owns_path(
            node.right, path, environment, visiting
        )
    if isinstance(node, ast.Name) and node.id in environment and node.id not in visiting:
        return expression_owns_path(
            environment[node.id], path, environment, visiting | frozenset({node.id})
        )
    if isinstance(node, ast.Call):
        function = node.func.id if isinstance(node.func, ast.Name) else ""
        if function == "glob":
            includes = string_values(node.args[0] if node.args else None, environment)
            excludes = set()
            for keyword in node.keywords:
                if keyword.arg == "exclude":
                    excludes |= string_values(keyword.value, environment)
            return any(bazel_glob_matches(pattern, path) for pattern in includes) and not any(
                bazel_glob_matches(pattern, path) for pattern in excludes
            )
        if function == "select":
            return any(expression_owns_path(argument, path, environment, visiting) for argument in node.args)
    return False


def call_name(node: ast.Call) -> str:
    return node.func.id if isinstance(node.func, ast.Name) else ""


def is_rust_rule(node: ast.Call) -> bool:
    function = call_name(node)
    return (
        function.startswith(("rust_", "ctx_rust_"))
        or function.endswith("_contract_test")
        or function.endswith("_binary_contract")
        or function.endswith("_integration_test")
    )


def rule_name(node: ast.Call) -> str | None:
    for keyword in node.keywords:
        if (
            keyword.arg == "name"
            and isinstance(keyword.value, ast.Constant)
            and isinstance(keyword.value.value, str)
        ):
            return keyword.value.value
    return None


def rust_rules_for_target(
    modules: list[ast.Module],
    path: str,
    target_name: str | None = None,
) -> list[tuple[ast.Call, dict[str, ast.AST]]]:
    result: list[tuple[ast.Call, dict[str, ast.AST]]] = []
    for module in modules:
        environment = assignments(module)
        for node in ast.walk(module):
            if not isinstance(node, ast.Call) or not is_rust_rule(node):
                continue
            if target_name is not None and rule_name(node) != target_name:
                continue
            keywords = {keyword.arg: keyword.value for keyword in node.keywords}
            if any(
                expression_owns_path(keywords.get(attribute), path, environment)
                for attribute in ("crate_root", "src", "srcs")
            ):
                result.append((node, environment))
    return result


def rust_source_owned(modules: list[ast.Module], path: str) -> bool:
    return bool(rust_rules_for_target(modules, path))


def bazel_path_declared(modules: list[ast.Module], path: str) -> bool:
    """Return whether a non-Rust Cargo input is structurally declared to Bazel."""
    for module in modules:
        environment = assignments(module)
        for node in ast.walk(module):
            if not isinstance(node, ast.Call):
                continue
            function = node.func.id if isinstance(node.func, ast.Name) else ""
            if function == "exports_files" and any(
                expression_owns_path(argument, path, environment) for argument in node.args
            ):
                return True
            keywords = {keyword.arg: keyword.value for keyword in node.keywords}
            if any(
                expression_owns_path(keywords.get(attribute), path, environment)
                for attribute in ("src", "srcs", "data")
            ):
                return True
    return False


def has_named_target(modules: list[ast.Module], name: str) -> bool:
    return any(
        isinstance(node, ast.Call)
        and any(
            keyword.arg == "name"
            and isinstance(keyword.value, ast.Constant)
            and keyword.value.value == name
            for keyword in node.keywords
        )
        for module in modules
        for node in ast.walk(module)
    )


def dependency_ownership(
    modules: list[ast.Module],
    *,
    target_name: str | None = None,
    target_path: str | None = None,
    tests_only: bool = False,
) -> tuple[set[str], set[str]]:
    labels: set[str] = set()
    flags: set[str] = set()
    for module in modules:
        environment = assignments(module)
        for node in ast.walk(module):
            if not isinstance(node, ast.Call) or not is_rust_rule(node):
                continue
            name = rule_name(node)
            if target_name is not None and name != target_name:
                continue
            function = call_name(node)
            if tests_only and not (
                function.endswith(("_test", "_contract_test"))
                or name == "unit_tests"
            ):
                continue
            if target_path is not None:
                keywords = {keyword.arg: keyword.value for keyword in node.keywords}
                if not any(
                    expression_owns_path(keywords.get(attribute), target_path, environment)
                    for attribute in ("crate_root", "src", "srcs")
                ):
                    continue
            for keyword in node.keywords:
                if keyword.arg not in {"deps", "proc_macro_deps"}:
                    continue
                labels |= {
                    value for value in string_values(keyword.value, environment) if value.startswith("//")
                }
                for call in ast.walk(keyword.value):
                    if not isinstance(call, ast.Call) or not isinstance(call.func, ast.Name):
                        continue
                    if call.func.id != "all_crate_deps":
                        continue
                    flags |= {
                        item.arg
                        for item in call.keywords
                        if item.arg is not None
                        and isinstance(item.value, ast.Constant)
                        and item.value.value is True
                    }
    return labels, flags


def expected_bazel_target_name(target: str) -> str | None:
    kind, name = target.split(":", 1)
    if kind == "custom-build":
        return None
    return "lib" if kind == "lib" else name


def assert_target_ownership(
    root: Path,
    package_name: str,
    package_dir: Path,
    data: dict[str, Any],
) -> int:
    modules = package_bazel_modules(root, package_dir)
    if not has_named_target(modules, "cargo_package_data"):
        fail(f"{package_name} BUILD.bazel has no cargo_package_data target")
    targets = cargo_targets(package_dir, data)
    root_modules = package_bazel_modules(root, root)
    for target, relative in targets.items():
        path = relative.as_posix()
        if not (package_dir / relative).is_file():
            fail(f"{package_name} {target} source is missing: {path}")
        expected_name = expected_bazel_target_name(target)
        owning_rules = (
            rust_rules_for_target(modules, path, expected_name)
            if expected_name is not None
            else []
        )
        owning_rules = [
            rule for rule in owning_rules if rule_executes_target(rule[0], target, data)
        ]
        if not owning_rules and target.startswith(("test:", "example:", "bench:")):
            owning_rules = [
                rule for rule in rust_rules_for_target(modules, path)
                if rule_executes_target(rule[0], target, data)
            ]
        owned = bool(owning_rules)
        if target == "custom-build:build-script-build":
            workspace_path = (package_dir.relative_to(root) / relative).as_posix()
            owned = owned or bazel_path_declared(modules, path) or bazel_path_declared(
                root_modules, workspace_path
            )
        if not owned:
            fail(f"{package_name} Cargo target is not owned by Bazel: {target} ({path})")
    return len(targets)


def rule_executes_target(node: ast.Call, target: str, data: dict[str, Any]) -> bool:
    kind, name = target.split(":", 1)
    function = call_name(node)
    if kind == "bin":
        return function in {"rust_binary", "ctx_rust_binary"}
    if kind != "test":
        return True

    harness = next(
        (item.get("harness", True) for item in explicit_targets(data, "test")
         if item.get("name") == name),
        True,
    )
    if function in LIBTEST_CONTRACT_RULES:
        return harness is True
    if function not in {"rust_test", "ctx_rust_test"}:
        return False
    # A harness=false Cargo test still needs a Bazel test action. A binary alone
    # only builds it, and the default libtest harness would replace its main.
    bazel_harness = next(
        (keyword.value for keyword in node.keywords if keyword.arg == "use_libtest_harness"),
        ast.Constant(value=True),
    )
    return isinstance(bazel_harness, ast.Constant) and bazel_harness.value is harness


def dependency_entries(data: dict[str, Any]) -> list[tuple[str, str, Any]]:
    result: list[tuple[str, str, Any]] = []
    for table in DEPENDENCY_TABLES:
        value = data.get(table, {})
        if not isinstance(value, dict):
            fail(f"[{table}] must be a table")
        result.extend((table, name, entry) for name, entry in value.items())
    target = data.get("target", {})
    if not isinstance(target, dict):
        fail("[target] must be a table")
    for target_data in target.values():
        if not isinstance(target_data, dict):
            fail("target-specific dependency configuration must be a table")
        for table in DEPENDENCY_TABLES:
            value = target_data.get(table, {})
            if not isinstance(value, dict):
                fail(f"target-specific [{table}] must be a table")
            result.extend((table, name, entry) for name, entry in value.items())
    return result


def workspace_dependencies(root: Path) -> dict[str, Any]:
    workspace = load_toml(root / "Cargo.toml").get("workspace")
    if not isinstance(workspace, dict):
        fail("root Cargo.toml must define [workspace]")
    dependencies = workspace.get("dependencies", {})
    if not isinstance(dependencies, dict):
        fail("[workspace.dependencies] must be a table")
    return dependencies


def resolved_local_dependency(
    root: Path,
    package_dir: Path,
    package_name: str,
    dependency_name: str,
    value: Any,
    inherited_dependencies: dict[str, Any],
) -> tuple[str, Path] | None:
    if not isinstance(value, dict):
        return None
    definition = value
    base = None
    if value.get("workspace") is True:
        if dependency_name not in inherited_dependencies:
            fail(
                f"{package_name} workspace dependency {dependency_name} is not defined "
                "by [workspace.dependencies]"
            )
        definition = inherited_dependencies[dependency_name]
        base = root
    if not isinstance(definition, dict) or "path" not in definition:
        return None
    path = definition["path"]
    if not isinstance(path, str):
        fail(f"{package_name} dependency {dependency_name} has a non-string path")
    canonical_name = definition.get("package", dependency_name)
    if not isinstance(canonical_name, str) or not canonical_name:
        fail(f"{package_name} dependency {dependency_name} has an invalid package name")
    return canonical_name, ((base or package_dir).resolve() / path).resolve()


def local_graph(
    root: Path,
    packages: dict[str, tuple[Path, dict[str, Any]]],
) -> dict[str, set[str]]:
    by_root = {directory.resolve(): name for name, (directory, _) in packages.items()}
    inherited_dependencies = workspace_dependencies(root)
    graph = {name: set() for name in packages}
    for name, (directory, data) in packages.items():
        modules = package_bazel_modules(root, directory)
        targets = cargo_targets(directory, data)
        for table, dependency_name, value in dependency_entries(data):
            resolved_dependency = resolved_local_dependency(
                root, directory, name, dependency_name, value, inherited_dependencies
            )
            if resolved_dependency is None:
                continue
            canonical_name, resolved = resolved_dependency
            target = by_root.get(resolved)
            if target is None:
                fail(
                    f"{name} dependency {dependency_name} escapes the workspace: "
                    f"{resolved_dependency[1]}"
                )
            manifest = load_toml(resolved / "Cargo.toml")
            package = manifest.get("package")
            actual_name = package.get("name") if isinstance(package, dict) else None
            if actual_name != canonical_name or target != canonical_name:
                fail(
                    f"{name} dependency {dependency_name} resolves to {actual_name!r}, "
                    f"not package {canonical_name!r}"
                )
            if table != "dev-dependencies":
                graph[name].add(target)
            package_label = f"//{resolved.relative_to(root).as_posix()}:"
            required_flag = {
                "dependencies": "normal",
                "dev-dependencies": "normal_dev",
                "build-dependencies": "build",
            }[table]
            checks: list[tuple[str, set[str], set[str]]] = []
            if table == "dependencies":
                for cargo_target, relative in targets.items():
                    if cargo_target.startswith(("test:", "custom-build:")):
                        continue
                    bazel_target = expected_bazel_target_name(cargo_target)
                    if bazel_target is None:
                        continue
                    labels, flags = dependency_ownership(
                        modules,
                        target_name=bazel_target,
                        target_path=relative.as_posix(),
                    )
                    checks.append((cargo_target, labels, flags))
            elif table == "dev-dependencies":
                labels, flags = dependency_ownership(modules, tests_only=True)
                checks.append(("test targets", labels, flags))
            else:
                labels, flags = dependency_ownership(modules)
                checks.append(("build targets", labels, flags))
            if not checks:
                fail(f"{name} has no Bazel {table} target for Cargo path dependency {target}")
            for cargo_target, dependency_labels, dependency_flags in checks:
                has_explicit_label = any(
                    label.startswith(package_label) for label in dependency_labels
                )
                protected_labels = VISIBILITY_RESTRICTED_LOCAL_LABELS.get(target)
                if protected_labels and not protected_labels & dependency_labels:
                    fail(
                        f"{name} Bazel {cargo_target} must explicitly declare "
                        "the visibility-restricted library label for "
                        f"Cargo path dependency {target}"
                    )
                if not has_explicit_label and required_flag not in dependency_flags:
                    fail(
                        f"{name} Bazel {cargo_target} omits Cargo path dependency {target}"
                    )
    return graph


def assert_acyclic(graph: dict[str, set[str]]) -> None:
    visiting: set[str] = set()
    visited: set[str] = set()

    def visit(package: str, stack: list[str]) -> None:
        if package in visiting:
            start = stack.index(package)
            fail(f"workspace dependency cycle: {' -> '.join(stack[start:] + [package])}")
        if package in visited:
            return
        visiting.add(package)
        stack.append(package)
        for dependency in sorted(graph[package]):
            visit(dependency, stack)
        stack.pop()
        visiting.remove(package)
        visited.add(package)

    for package in sorted(graph):
        visit(package, [])


def main() -> None:
    root = repository_root()
    packages = workspace_packages(root)
    target_count = sum(
        assert_target_ownership(root, name, directory, data)
        for name, (directory, data) in packages.items()
    )
    graph = local_graph(root, packages)
    assert_acyclic(graph)
    edge_count = sum(map(len, graph.values()))
    print(
        f"live Cargo/Bazel ownership covers {target_count} Cargo targets and "
        f"{edge_count} local edges across {len(packages)} discovered packages"
    )


if __name__ == "__main__":
    try:
        main()
    except (InventoryError, OSError) as error:
        print(f"rust target inventory check failed: {error}", file=sys.stderr)
        raise SystemExit(1) from None
