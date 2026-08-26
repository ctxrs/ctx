#!/usr/bin/env python3
"""Static and evaluated ownership boundary for ctx-history-providers-task-docs."""

from __future__ import annotations

import ast
import os
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Iterator, Sequence


PACK_PACKAGE = "ctx-history-providers-task-docs"
PACK_LABEL = "//crates/ctx-history-providers-task-docs:lib"
LIVE_BOUNDARY_TARGET = "//:history_provider_pack_boundary_check"
MUTATION_BOUNDARY_TARGET = "//tools/bazel:history_provider_pack_boundary_mutation_tests"
CAPTURE_PACKAGE = "ctx-history-capture-composition"
CAPTURE_BUILD_LABEL = "//crates/ctx-history-capture-composition:BUILD.bazel"
EVALUATED_REVERSE_BAZEL_CONSUMERS = {
    PACK_LABEL: (
        "//crates/ctx-history-capture-composition:lib",
        "//crates/ctx-history-capture-composition:test_support_lib",
        "//crates/ctx-history-capture-composition:unit_tests",
        "//crates/ctx-history-providers-task-docs:lib",
    ),
}
EXPECTED_PACK_DEPENDENCIES = {
    "chrono": {"workspace": True},
    "libc": {"workspace": True},
    "rusqlite": {"workspace": True},
    "serde": {"workspace": True},
    "serde_json": {"workspace": True},
    "sha2": {"workspace": True},
    "thiserror": {"workspace": True},
    "ctx-history-capture-model": {"path": "../ctx-history-capture-model"},
    "ctx-history-capture-runtime": {"path": "../ctx-history-capture-runtime"},
    "ctx-history-core": {"path": "../ctx-history-core"},
    "ctx-history-source-io": {"path": "../ctx-history-source-io"},
    "ctx-history-source-sqlite": {"path": "../ctx-history-source-sqlite"},
}
EXPECTED_PACK_DEV_DEPENDENCIES = {
    "tempfile": {"workspace": True},
}
PACK_DIRECT_BAZEL_DEPENDENCIES = (
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-capture-runtime:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-source-io:lib",
    "//crates/ctx-history-source-sqlite:lib",
)
DEPENDENCY_TABLE_NAMES = {"dependencies", "dev-dependencies", "build-dependencies"}
CANONICAL_LOAD_BINDINGS = {
    "@crates//:defs.bzl": {"aliases", "all_crate_deps", "crate_edition"},
    "@rules_rust//rust:defs.bzl": {"rust_library"},
    "//:rust_sources.bzl": {"RUST_PROD_SRC_EXCLUDES"},
    "//tools/bazel:ctx_rust.bzl": {"ctx_rust_test"},
}
CANONICAL_SYMBOL_SOURCES = {
    symbol: source
    for source, symbols in CANONICAL_LOAD_BINDINGS.items()
    for symbol in symbols
}
ALLOWED_BAZEL_CALLS = {
    "aliases",
    "all_crate_deps",
    "crate_edition",
    "ctx_rust_test",
    "filegroup",
    "glob",
    "load",
    "package",
    "rust_library",
}
PACK_DEPENDENCY_VARIABLE = "TASK_DOCS_DEPS"


class BoundaryError(ValueError):
    pass


@dataclass(frozen=True)
class Token:
    kind: str
    value: str


def _dependency_tables(
    manifest: dict[str, Any], package: str
) -> Iterator[tuple[str, dict[str, Any]]]:
    unexpected_top_level = sorted(
        name
        for name in manifest
        if name.endswith("dependencies") and name not in DEPENDENCY_TABLE_NAMES
    )
    if unexpected_top_level:
        raise BoundaryError(
            f"{package} Cargo has unsupported dependency tables: "
            + ", ".join(unexpected_top_level)
        )
    for table_name in DEPENDENCY_TABLE_NAMES:
        table = manifest.get(table_name, {})
        if not isinstance(table, dict):
            raise BoundaryError(f"{package} Cargo {table_name} table must be a table")
        yield table_name, table

    target = manifest.get("target", {})
    if not isinstance(target, dict):
        raise BoundaryError(f"{package} Cargo target table must be a table")
    for target_name, target_tables in target.items():
        if not isinstance(target_tables, dict):
            raise BoundaryError(
                f"{package} Cargo target {target_name!r} table must be a table"
            )
        unexpected = sorted(set(target_tables) - DEPENDENCY_TABLE_NAMES)
        if unexpected:
            raise BoundaryError(
                f"{package} Cargo target {target_name!r} has unsupported tables: "
                + ", ".join(unexpected)
            )
        for table_name in DEPENDENCY_TABLE_NAMES:
            if table_name not in target_tables:
                continue
            table = target_tables[table_name]
            if not isinstance(table, dict):
                raise BoundaryError(
                    f"{package} Cargo target {target_name!r} {table_name} table "
                    "must be a table"
                )
            yield f"target.{target_name}.{table_name}", table


def _read_manifest(path: Path) -> dict[str, Any]:
    with path.open("rb") as handle:
        manifest = tomllib.load(handle)
    if not isinstance(manifest, dict):
        raise BoundaryError("Cargo manifest must be a table")
    return manifest


def _workspace_dependencies(workspace_manifest: dict[str, Any]) -> dict[str, Any]:
    workspace = workspace_manifest.get("workspace")
    if not isinstance(workspace, dict):
        raise BoundaryError("root Cargo manifest must define a workspace table")
    dependencies = workspace.get("dependencies", {})
    if not isinstance(dependencies, dict):
        raise BoundaryError("root Cargo workspace.dependencies must be a table")
    return dependencies


def _workspace_members(workspace_manifest: dict[str, Any]) -> tuple[str, ...]:
    workspace = workspace_manifest.get("workspace")
    if not isinstance(workspace, dict):
        raise BoundaryError("root Cargo manifest must define a workspace table")
    members = workspace.get("members")
    if not isinstance(members, list) or not all(
        isinstance(member, str) and member for member in members
    ):
        raise BoundaryError(
            "root Cargo workspace.members must be a list of non-empty strings"
        )
    if len(members) != len(set(members)):
        raise BoundaryError("root Cargo workspace.members has duplicate entries")
    return tuple(members)


def _canonical_dependency_name(
    dependency_key: str,
    specification: Any,
    package: str,
    table_name: str,
    workspace_dependencies: dict[str, Any],
    *,
    workspace_entry: bool = False,
) -> str:
    context = f"{package} Cargo {table_name} dependency {dependency_key!r}"
    if not isinstance(dependency_key, str) or not dependency_key:
        raise BoundaryError(f"{context} has an invalid dependency key")
    if isinstance(specification, str):
        return dependency_key
    if not isinstance(specification, dict):
        raise BoundaryError(f"{context} must be a string or inline table")

    package_name = specification.get("package")
    if package_name is not None and (
        not isinstance(package_name, str) or not package_name
    ):
        raise BoundaryError(f"{context} has an invalid package rename")

    workspace_inherited = specification.get("workspace")
    if workspace_inherited is not None and not isinstance(workspace_inherited, bool):
        raise BoundaryError(f"{context} has a non-boolean workspace inheritance flag")
    if workspace_entry and workspace_inherited is not None:
        raise BoundaryError(f"{context} cannot inherit from workspace.dependencies")
    if workspace_inherited is False:
        raise BoundaryError(f"{context} has an ambiguous workspace = false entry")
    if workspace_inherited is True:
        if package_name is not None:
            raise BoundaryError(
                f"{context} cannot combine workspace inheritance with a package rename"
            )
        inherited = workspace_dependencies.get(dependency_key)
        if inherited is None:
            raise BoundaryError(f"{context} is absent from root workspace.dependencies")
        return _canonical_dependency_name(
            dependency_key,
            inherited,
            "root workspace",
            "dependencies",
            workspace_dependencies,
            workspace_entry=True,
        )
    return package_name or dependency_key


def _canonical_dependency_names(
    manifest: dict[str, Any], package: str, workspace_manifest: dict[str, Any]
) -> set[str]:
    workspace_dependencies = _workspace_dependencies(workspace_manifest)
    names: set[str] = set()
    for table_name, table in _dependency_tables(manifest, package):
        for dependency_key, specification in table.items():
            names.add(
                _canonical_dependency_name(
                    dependency_key,
                    specification,
                    package,
                    table_name,
                    workspace_dependencies,
                )
            )
    return names


def _forbidden_pack_dependencies(
    manifest: dict[str, Any], workspace_manifest: dict[str, Any]
) -> list[str]:
    names = _canonical_dependency_names(manifest, PACK_PACKAGE, workspace_manifest)
    return sorted(
        name
        for name in names
        if name == CAPTURE_PACKAGE
        or name.startswith("ctx-history-index")
        or (name.startswith("ctx-history-providers-") and name != PACK_PACKAGE)
    )


def _validate_pack_manifest(manifest_path: Path, workspace_manifest: dict[str, Any]) -> None:
    manifest = _read_manifest(manifest_path)
    forbidden = _forbidden_pack_dependencies(manifest, workspace_manifest)
    if forbidden:
        raise BoundaryError(
            f"{PACK_PACKAGE} has forbidden Cargo dependencies: " + ", ".join(forbidden)
        )

    dependencies = manifest.get("dependencies", {})
    if dependencies != EXPECTED_PACK_DEPENDENCIES:
        raise BoundaryError(
            f"{PACK_PACKAGE} Cargo production dependencies drifted: "
            f"expected={sorted(EXPECTED_PACK_DEPENDENCIES)} actual={sorted(dependencies)}"
        )
    if manifest.get("dev-dependencies", {}) != EXPECTED_PACK_DEV_DEPENDENCIES:
        raise BoundaryError(
            f"{PACK_PACKAGE} Cargo dev dependencies drifted: "
            f"expected={sorted(EXPECTED_PACK_DEV_DEPENDENCIES)} "
            f"actual={sorted(manifest.get('dev-dependencies', {}))}"
        )
    if manifest.get("build-dependencies", {}):
        raise BoundaryError(f"{PACK_PACKAGE} Cargo build dependencies drifted")

    target_dependency_tables = [
        table_name
        for table_name, _ in _dependency_tables(manifest, PACK_PACKAGE)
        if table_name.startswith("target.")
    ]
    if target_dependency_tables:
        raise BoundaryError(
            f"{PACK_PACKAGE} Cargo target-specific dependency-table bypass: "
            + ", ".join(target_dependency_tables)
        )


def _validate_reverse_cargo_consumers(
    workspace_manifest: dict[str, Any], member_paths: Sequence[Path]
) -> None:
    reverse: list[tuple[str, str]] = []
    for path in member_paths:
        manifest = _read_manifest(path)
        package = manifest.get("package", {})
        consumer = package.get("name")
        if not isinstance(consumer, str) or not consumer:
            continue
        for table_name, table in _dependency_tables(manifest, consumer):
            for dependency_key, specification in table.items():
                if (
                    _canonical_dependency_name(
                        dependency_key,
                        specification,
                        consumer,
                        table_name,
                        _workspace_dependencies(workspace_manifest),
                    )
                    == PACK_PACKAGE
                ):
                    reverse.append((consumer, table_name))
    if sorted(reverse) != [(CAPTURE_PACKAGE, "dependencies")]:
        raise BoundaryError(
            f"{PACK_PACKAGE} reverse Cargo consumers drifted: {sorted(reverse)}"
        )


def _tokenize_starlark(source: str, package: str) -> list[Token]:
    tokens: list[Token] = []
    index = 0
    while index < len(source):
        character = source[index]
        if character.isspace():
            index += 1
            continue
        if character == "#":
            newline = source.find("\n", index)
            index = len(source) if newline == -1 else newline + 1
            continue
        if character in {"'", '"'}:
            start = index
            quote = character
            index += 1
            escaped = False
            while index < len(source):
                current = source[index]
                index += 1
                if escaped:
                    escaped = False
                elif current == "\\":
                    escaped = True
                elif current == quote:
                    break
                elif current == "\n":
                    raise BoundaryError(f"{package} Bazel has an unterminated string")
            else:
                raise BoundaryError(f"{package} Bazel has an unterminated string")
            try:
                value = ast.literal_eval(source[start:index])
            except (SyntaxError, ValueError) as error:
                raise BoundaryError(f"{package} Bazel has an invalid string literal") from error
            if not isinstance(value, str):
                raise BoundaryError(f"{package} Bazel string literal must be text")
            tokens.append(Token("string", value))
            continue
        if character.isalpha() or character == "_":
            start = index
            index += 1
            while index < len(source) and (
                source[index].isalnum() or source[index] == "_"
            ):
                index += 1
            tokens.append(Token("identifier", source[start:index]))
            continue
        tokens.append(Token("symbol", character))
        index += 1
    return tokens


def _split_top_level(tokens: Sequence[Token]) -> list[list[Token]]:
    parts: list[list[Token]] = [[]]
    depth = 0
    pairs = {"(": ")", "[": "]", "{": "}"}
    closing = set(pairs.values())
    for token in tokens:
        if token.value in pairs:
            depth += 1
        elif token.value in closing:
            depth -= 1
            if depth < 0:
                raise BoundaryError("Bazel has unbalanced delimiters")
        if token.value == "," and depth == 0:
            parts.append([])
        else:
            parts[-1].append(token)
    if depth != 0:
        raise BoundaryError("Bazel has unbalanced delimiters")
    return [part for part in parts if part]


def _find_calls(tokens: Sequence[Token], rule: str, package: str) -> list[list[Token]]:
    calls: list[list[Token]] = []
    for index, token in enumerate(tokens[:-1]):
        if (
            token.kind != "identifier"
            or token.value != rule
            or tokens[index + 1].value != "("
        ):
            continue
        depth = 0
        for end in range(index + 1, len(tokens)):
            value = tokens[end].value
            if value == "(":
                depth += 1
            elif value == ")":
                depth -= 1
                if depth == 0:
                    calls.append(list(tokens[index + 2 : end]))
                    break
                if depth < 0:
                    raise BoundaryError(f"{package} Bazel has unbalanced delimiters")
        else:
            raise BoundaryError(f"{package} Bazel has an unterminated {rule} call")
    return calls


def _named_arguments(call: Sequence[Token], package: str) -> dict[str, list[Token]]:
    arguments: dict[str, list[Token]] = {}
    for part in _split_top_level(call):
        if len(part) < 3 or part[0].kind != "identifier" or part[1].value != "=":
            raise BoundaryError(f"{package} Bazel target has an unsupported argument")
        name = part[0].value
        if name in arguments:
            raise BoundaryError(f"{package} Bazel target repeats {name}")
        arguments[name] = part[2:]
    return arguments


def _literal_string_list(tokens: Sequence[Token], package: str, name: str) -> tuple[str, ...]:
    if len(tokens) < 2 or tokens[0].value != "[" or tokens[-1].value != "]":
        raise BoundaryError(f"{package} Bazel {name} must be a literal string list")
    values: list[str] = []
    for item in _split_top_level(tokens[1:-1]):
        if len(item) != 1 or item[0].kind != "string":
            raise BoundaryError(f"{package} Bazel {name} must contain only string labels")
        values.append(item[0].value)
    if len(values) != len(set(values)):
        raise BoundaryError(f"{package} Bazel {name} has duplicate labels")
    return tuple(values)


def _literal_string_list_plus_identifiers(
    tokens: Sequence[Token], package: str, name: str, identifiers: Sequence[str]
) -> tuple[str, ...]:
    depth = 0
    plus: list[int] = []
    for index, token in enumerate(tokens):
        if token.value in {"[", "(", "{"}:
            depth += 1
        elif token.value in {"]", ")", "}"}:
            depth -= 1
        elif token.value == "+" and depth == 0:
            plus.append(index)
    segments: list[Sequence[Token]] = []
    start = 0
    for index in (*plus, len(tokens)):
        segments.append(tokens[start:index])
        start = index + 1
    if len(segments) != len(identifiers) + 1:
        raise BoundaryError(
            f"{package} Bazel {name} must be a literal string list plus "
            + ", ".join(identifiers)
        )
    for segment, identifier in zip(segments[1:], identifiers):
        if (
            len(segment) != 1
            or segment[0].kind != "identifier"
            or segment[0].value != identifier
        ):
            raise BoundaryError(
                f"{package} Bazel {name} must be a literal string list plus "
                + ", ".join(identifiers)
            )
    return _literal_string_list(segments[0], package, name)


def _assignments(tokens: Sequence[Token], name: str) -> list[tuple[str, list[Token]]]:
    assignments: list[tuple[str, list[Token]]] = []
    for index, token in enumerate(tokens):
        if token.kind != "identifier" or token.value != name:
            continue
        if index + 1 < len(tokens) and tokens[index + 1].value == "=":
            operator = "="
            value = tokens[index + 2 :]
        elif (
            index + 2 < len(tokens)
            and tokens[index + 1].value == "+"
            and tokens[index + 2].value == "="
        ):
            operator = "+="
            value = tokens[index + 3 :]
        else:
            continue
        if not value or value[0].value != "[":
            assignments.append((operator, value[:1]))
            continue
        depth = 0
        for end, value_token in enumerate(value):
            if value_token.value == "[":
                depth += 1
            elif value_token.value == "]":
                depth -= 1
                if depth == 0:
                    assignments.append((operator, list(value[: end + 1])))
                    break
        else:
            raise BoundaryError(f"Bazel {name} assignment has unbalanced delimiters")
    return assignments


def _validate_canonical_loads(
    tokens: Sequence[Token], package: str, reserved_symbols: set[str]
) -> None:
    actual_bindings: dict[str, set[str]] = {}
    for call in _find_calls(tokens, "load", package):
        arguments = _split_top_level(call)
        if not arguments or len(arguments[0]) != 1 or arguments[0][0].kind != "string":
            raise BoundaryError(f"{package} Bazel has an unsupported load source")
        source = arguments[0][0].value
        if source in actual_bindings:
            raise BoundaryError(f"{package} Bazel must load {source} exactly once")
        imported_names: list[str] = []
        for imported in arguments[1:]:
            if len(imported) == 1 and imported[0].kind == "string":
                local_name = imported[0].value
                remote_name = local_name
            elif (
                len(imported) == 3
                and imported[0].kind == "identifier"
                and imported[1].value == "="
                and imported[2].kind == "string"
            ):
                local_name = imported[0].value
                remote_name = imported[2].value
                raise BoundaryError(
                    f"{package} Bazel load aliases are unsupported; {remote_name!r} "
                    "must be loaded without aliasing"
                )
            else:
                raise BoundaryError(f"{package} Bazel has an unsupported load binding")

            if local_name in reserved_symbols or remote_name in reserved_symbols:
                raise BoundaryError(
                    f"{package} Bazel {remote_name} must be a local literal and may not be loaded"
                )
            expected_source = CANONICAL_SYMBOL_SOURCES.get(remote_name)
            if expected_source is not None and source != expected_source:
                raise BoundaryError(
                    f"{package} Bazel trusted symbol {remote_name!r} must be loaded "
                    "from its canonical source"
                )
            imported_names.append(remote_name)

        expected_bindings = CANONICAL_LOAD_BINDINGS.get(source)
        if expected_bindings is None:
            raise BoundaryError(f"{package} Bazel has an unsupported load source: {source}")
        actual = set(imported_names)
        if actual != expected_bindings:
            raise BoundaryError(
                f"{package} Bazel {source} load bindings drifted: "
                f"expected={sorted(expected_bindings)} actual={sorted(actual)}"
            )
        actual_bindings[source] = actual

    missing_sources = sorted(set(CANONICAL_LOAD_BINDINGS) - set(actual_bindings))
    if missing_sources:
        raise BoundaryError(
            f"{package} Bazel is missing canonical loads: " + ", ".join(missing_sources)
        )


def _validate_call_surface(tokens: Sequence[Token], package: str) -> None:
    for index, token in enumerate(tokens):
        if token.value != "(":
            continue
        caller = tokens[index - 1] if index else None
        if (
            caller is None
            or caller.kind != "identifier"
            or caller.value not in ALLOWED_BAZEL_CALLS
        ):
            name = caller.value if caller is not None else "<expression>"
            raise BoundaryError(
                f"{package} Bazel has an unsupported rule or macro call: {name}"
            )

    for index, token in enumerate(tokens):
        if token.kind != "identifier" or token.value not in ALLOWED_BAZEL_CALLS:
            continue
        previous = tokens[index - 1].value if index else None
        following = tokens[index + 1].value if index + 1 < len(tokens) else None
        if token.value == "aliases" and following == "=" and previous in {"(", ","}:
            continue
        if following != "(" or previous in {".", "def"}:
            raise BoundaryError(
                f"{package} Bazel rule or macro symbol {token.value} may not be rebound "
                "or referenced through an alias"
            )


def _is_all_crate_deps(tokens: Sequence[Token], **expected: str) -> bool:
    if len(tokens) < 3 or tokens[0].value != "all_crate_deps" or tokens[1].value != "(":
        return False
    if tokens[-1].value != ")":
        return False
    actual: dict[str, str] = {}
    for argument in _split_top_level(tokens[2:-1]):
        if (
            len(argument) != 3
            or argument[0].kind != "identifier"
            or argument[1].value != "="
            or argument[2].kind != "identifier"
        ):
            return False
        actual[argument[0].value] = argument[2].value
    return actual == expected


def _validate_dependency_expression(
    tokens: Sequence[Token],
    package: str,
    context: str,
    generated_flags: dict[str, str],
) -> None:
    plus = [index for index, token in enumerate(tokens) if token.value == "+"]
    valid = (
        len(plus) == 1
        and _is_all_crate_deps(tokens[: plus[0]], **generated_flags)
        and [token.value for token in tokens[plus[0] + 1 :]] == [PACK_DEPENDENCY_VARIABLE]
    )
    if not valid:
        rendered_flags = ", ".join(
            f"{name} = {value}" for name, value in generated_flags.items()
        )
        raise BoundaryError(
            f"{package} Bazel {context} must be exactly "
            f"all_crate_deps({rendered_flags}) + {PACK_DEPENDENCY_VARIABLE}"
        )


def _validate_rule_dependencies(
    call: Sequence[Token],
    package: str,
    context: str,
    dependency_flags: dict[str, str],
    proc_macro_flags: dict[str, str],
) -> dict[str, list[Token]]:
    arguments = _named_arguments(call, package)
    name = arguments.get("name")
    if name is None or len(name) != 1 or name[0].kind != "string":
        raise BoundaryError(f"{package} Bazel {context} must have a literal name")
    _validate_dependency_expression(arguments.get("deps", []), package, f"{context} deps", dependency_flags)
    if not _is_all_crate_deps(arguments.get("proc_macro_deps", []), **proc_macro_flags):
        rendered_flags = ", ".join(
            f"{name} = {value}" for name, value in proc_macro_flags.items()
        )
        raise BoundaryError(
            f"{package} Bazel {context} proc_macro_deps must be exactly "
            f"all_crate_deps({rendered_flags})"
        )
    return arguments


def _validate_pack_bazel(build_path: Path) -> None:
    package = PACK_PACKAGE
    tokens = _tokenize_starlark(build_path.read_text(encoding="utf-8"), package)
    _validate_canonical_loads(tokens, package, {PACK_DEPENDENCY_VARIABLE})
    _validate_call_surface(tokens, package)

    assignments = _assignments(tokens, PACK_DEPENDENCY_VARIABLE)
    if len(assignments) != 1 or assignments[0][0] != "=":
        raise BoundaryError(
            f"{package} Bazel {PACK_DEPENDENCY_VARIABLE} may only be assigned once"
        )
    if _literal_string_list(assignments[0][1], package, PACK_DEPENDENCY_VARIABLE) != PACK_DIRECT_BAZEL_DEPENDENCIES:
        raise BoundaryError(
            f"{package} Bazel {PACK_DEPENDENCY_VARIABLE} drifted: "
            f"expected={list(PACK_DIRECT_BAZEL_DEPENDENCIES)} "
            f"actual={list(_literal_string_list(assignments[0][1], package, PACK_DEPENDENCY_VARIABLE))}"
        )

    actual_uses = sum(
        token.kind == "identifier" and token.value == PACK_DEPENDENCY_VARIABLE
        for token in tokens
    )
    if actual_uses != 3:
        raise BoundaryError(
            f"{package} Bazel {PACK_DEPENDENCY_VARIABLE} may only be assigned once and used directly by dependency attributes"
        )

    rust_libraries = _find_calls(tokens, "rust_library", package)
    if len(rust_libraries) != 1:
        raise BoundaryError(f"{package} Bazel must define exactly one rust_library target")
    library_arguments = _validate_rule_dependencies(
        rust_libraries[0],
        package,
        "rust_library",
        {"normal": "True"},
        {"proc_macro": "True"},
    )
    if library_arguments["name"][0].value != "lib":
        raise BoundaryError(f"{package} Bazel rust_library must be named lib")

    rust_tests = _find_calls(tokens, "ctx_rust_test", package)
    if len(rust_tests) != 1:
        raise BoundaryError(f"{package} Bazel must define exactly one ctx_rust_test")
    test_arguments = _validate_rule_dependencies(
        rust_tests[0],
        package,
        "ctx_rust_test",
        {"normal": "True", "normal_dev": "True"},
        {"proc_macro": "True", "proc_macro_dev": "True"},
    )
    if test_arguments["name"][0].value != "unit_tests":
        raise BoundaryError(f"{package} Bazel ctx_rust_test must be named unit_tests")


def _validate_cargo_workspace_package_data(
    root_tokens: Sequence[Token], workspace_manifest: dict[str, Any]
) -> None:
    package = "root Cargo workspace package data"
    assignments = _assignments(root_tokens, "CARGO_WORKSPACE_PACKAGE_DATA")
    if len(assignments) != 1 or assignments[0][0] != "=":
        raise BoundaryError(
            "CARGO_WORKSPACE_PACKAGE_DATA must be assigned exactly once"
        )
    actual = set(
        _literal_string_list(
            assignments[0][1], package, "CARGO_WORKSPACE_PACKAGE_DATA"
        )
    )
    expected = {
        f"//{member}:cargo_package_data"
        for member in _workspace_members(workspace_manifest)
    }
    if actual != expected:
        missing = sorted(expected - actual)
        stale = sorted(actual - expected)
        raise BoundaryError(
            "Cargo workspace package-data closure drifted: "
            f"missing={missing} stale={stale}"
        )


def _validate_live_gate_registration(
    root_build_path: Path,
    tools_build_path: Path,
    workspace_manifest: dict[str, Any],
) -> None:
    package = "root provider-pack boundary"
    root_tokens = _tokenize_starlark(
        root_build_path.read_text(encoding="utf-8"), package
    )
    _validate_cargo_workspace_package_data(root_tokens, workspace_manifest)
    matching: list[dict[str, list[Token]]] = []
    for call in _find_calls(root_tokens, "sh_test", package):
        arguments = _named_arguments(call, package)
        name = arguments.get("name", [])
        if (
            len(name) == 1
            and name[0].kind == "string"
            and name[0].value == LIVE_BOUNDARY_TARGET.removeprefix("//:")
        ):
            matching.append(arguments)
    if len(matching) != 1:
        raise BoundaryError(
            f"root BUILD must define exactly one {LIVE_BOUNDARY_TARGET} sh_test"
        )

    arguments = matching[0]
    if _literal_string_list(arguments.get("srcs", []), package, "srcs") != (
        "//tools/bazel:check-history-provider-pack-boundary.sh",
    ):
        raise BoundaryError(f"{LIVE_BOUNDARY_TARGET} source registration drifted")
    if _literal_string_list(arguments.get("args", []), package, "args") != (
        "$(rootpath :history_provider_pack_boundary_root_build)",
    ):
        raise BoundaryError(f"{LIVE_BOUNDARY_TARGET} root BUILD argument drifted")
    if _literal_string_list(arguments.get("data", []), package, "data") != (
        ":history_provider_pack_boundary_root_build",
        ":history_provider_pack_boundary_inputs",
    ):
        raise BoundaryError(
            f"{LIVE_BOUNDARY_TARGET} complete Cargo/BUILD data registration drifted"
        )
    if _literal_string_list(arguments.get("tags", []), package, "tags") != (
        "build-graph",
        "exclusive",
        "no-cache",
    ):
        raise BoundaryError(f"{LIVE_BOUNDARY_TARGET} governed tags drifted")

    input_groups = []
    for call in _find_calls(root_tokens, "filegroup", package):
        candidate = _named_arguments(call, package)
        name = candidate.get("name", [])
        if (
            len(name) == 1
            and name[0].kind == "string"
            and name[0].value == "history_provider_pack_boundary_inputs"
        ):
            input_groups.append(candidate)
    if len(input_groups) != 1:
        raise BoundaryError("root provider-pack boundary must define one input filegroup")
    if _literal_string_list_plus_identifiers(
        input_groups[0].get("srcs", []),
        package,
        "input filegroup srcs",
        ("CARGO_WORKSPACE_PACKAGE_DATA",),
    ) != (
        "scripts/bazelw",
    ):
        raise BoundaryError("root provider-pack boundary input filegroup drifted")
    if _literal_string_list(
        input_groups[0].get("visibility", []), package, "input filegroup visibility"
    ) != ("//visibility:public",):
        raise BoundaryError("root provider-pack boundary input filegroup must be public")

    tools_tokens = _tokenize_starlark(
        tools_build_path.read_text(encoding="utf-8"), "provider-pack boundary tools"
    )
    mutation_gates: list[dict[str, list[Token]]] = []
    for call in _find_calls(tools_tokens, "py_test", "provider-pack boundary tools"):
        candidate = _named_arguments(call, "provider-pack boundary tools")
        name = candidate.get("name", [])
        if (
            len(name) == 1
            and name[0].kind == "string"
            and name[0].value == MUTATION_BOUNDARY_TARGET.rsplit(":", 1)[1]
        ):
            mutation_gates.append(candidate)
    if len(mutation_gates) != 1:
        raise BoundaryError(f"{MUTATION_BOUNDARY_TARGET} must define exactly one py_test")
    mutation_gate = mutation_gates[0]
    if _literal_string_list(mutation_gate.get("data", []), package, "mutation data") != (
        "//:history_provider_pack_boundary_inputs",
    ):
        raise BoundaryError(f"{MUTATION_BOUNDARY_TARGET} complete Cargo/BUILD data drifted")
    mutation_tag_tokens = mutation_gate.get("tags")
    mutation_tags = (
        _literal_string_list(mutation_tag_tokens, package, "mutation tags")
        if mutation_tag_tokens is not None
        else ()
    )
    if {"manual", "tier-nightly", "tier-release"}.intersection(mutation_tags):
        raise BoundaryError(f"{MUTATION_BOUNDARY_TARGET} must remain in default CI")


def _validate_reverse_build_inventory(
    capture_build: Path,
    member_builds: Sequence[Path],
) -> None:
    expected = {
        path.resolve(): (() if path.resolve() != capture_build.resolve() else (PACK_LABEL, PACK_LABEL))
        for path in member_builds
    }
    for path in member_builds:
        tokens = _tokenize_starlark(path.read_text(encoding="utf-8"), str(path))
        actual = tuple(
            token.value
            for token in tokens
            if token.kind == "string" and token.value == PACK_LABEL
        )
        if tuple(sorted(actual)) != tuple(sorted(expected[path.resolve()])):
            raise BoundaryError(
                f"unexpected reverse {PACK_PACKAGE} Bazel consumer in {path}: "
                f"expected={list(expected[path.resolve()])} actual={list(actual)}"
            )


def validate_evaluated_reverse_bazel_consumers(
    query: Callable[[str], Sequence[str]],
) -> None:
    for target, expected in EVALUATED_REVERSE_BAZEL_CONSUMERS.items():
        actual = tuple(sorted(query(f"rdeps(//..., {target}, 1)")))
        if actual != expected:
            raise BoundaryError(
                f"{PACK_PACKAGE} evaluated reverse Bazel consumers drifted: "
                f"target={target} expected={list(expected)} actual={list(actual)}"
            )


def _validate_live_reverse_bazel_consumers(workspace_path: Path) -> None:
    repo_root = workspace_path.parent.resolve()
    bazel_wrapper = repo_root / "scripts/bazelw"
    if not bazel_wrapper.is_file() or not os.access(bazel_wrapper, os.X_OK):
        raise BoundaryError(
            f"{PACK_PACKAGE} boundary requires an executable scripts/bazelw"
        )

    with tempfile.TemporaryDirectory(prefix="ctx-provider-pack-boundary-", dir="/tmp") as scratch:
        scratch_path = Path(scratch)
        environment = os.environ.copy()
        environment.pop("BUILD_WORKSPACE_DIRECTORY", None)
        environment.update(
            {
                "HOME": str(scratch_path / "home"),
                "BAZEL_OUTPUT_USER_ROOT": str(scratch_path / "bazel-output"),
                "CTX_BAZEL_SANDBOX_BASE": str(scratch_path / "bazel-sandboxes"),
                "CTX_BAZEL_WORKSPACE": str(repo_root),
            }
        )
        (scratch_path / "home").mkdir()

        def query(expression: str) -> tuple[str, ...]:
            result = subprocess.run(
                [str(bazel_wrapper), "query", expression, "--output=label"],
                check=False,
                capture_output=True,
                cwd=repo_root,
                env=environment,
                text=True,
            )
            if result.returncode != 0:
                raise BoundaryError(result.stderr.strip() or result.stdout.strip())
            return tuple(
                line.strip()
                for line in result.stdout.splitlines()
                if line.strip().startswith("//")
            )

        validate_evaluated_reverse_bazel_consumers(query)


def validate(
    workspace_manifest_path: Path,
    pack_manifest_path: Path,
    pack_build_path: Path,
    capture_manifest_path: Path,
    capture_build_path: Path,
    member_cargos: Sequence[Path],
    member_builds: Sequence[Path],
) -> None:
    repo_root = workspace_manifest_path.parent
    workspace_manifest = _read_manifest(workspace_manifest_path)
    _validate_live_gate_registration(
        repo_root / "BUILD.bazel",
        repo_root / "tools/bazel/BUILD.bazel",
        workspace_manifest,
    )
    _validate_pack_manifest(pack_manifest_path, workspace_manifest)
    _validate_pack_bazel(pack_build_path)
    _validate_reverse_cargo_consumers(workspace_manifest, member_cargos)
    _validate_reverse_build_inventory(capture_build_path, member_builds)


def main() -> int:
    if sys.argv.count("--member-builds") != 1:
        print(
            "usage: check_history_provider_pack_boundary.py WORKSPACE_CARGO PACK_CARGO "
            "PACK_BUILD CAPTURE_CARGO CAPTURE_BUILD MEMBER_CARGO... --member-builds BUILD...",
            file=sys.stderr,
        )
        return 64
    build_separator = sys.argv.index("--member-builds")
    if build_separator < 6 or build_separator == len(sys.argv) - 1:
        print("provider pack boundary requires Cargo and BUILD inventories", file=sys.stderr)
        return 64
    try:
        validate(
            *(Path(argument) for argument in sys.argv[1:6]),
            tuple(Path(argument) for argument in sys.argv[6:build_separator]),
            tuple(Path(argument) for argument in sys.argv[build_separator + 1 :]),
        )
        _validate_live_reverse_bazel_consumers(Path(sys.argv[1]))
    except (BoundaryError, OSError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    print("ctx-history-providers-task-docs static and evaluated ownership boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
