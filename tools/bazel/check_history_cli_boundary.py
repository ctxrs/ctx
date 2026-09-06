#!/usr/bin/env python3
"""Fail-closed static boundary for ctx-history-cli's final CLI seam."""

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


HISTORY_PACKAGE = "ctx-history-cli"
FINAL_PACKAGE = "ctx"
FORBIDDEN_HISTORY_CARGO = {"clap", "ctx-cli"}
HISTORY_DEPS = (
    "//crates/ctx-history-capture:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-platform:lib",
    "//crates/ctx-history-ingest-application:lib",
    "//crates/ctx-history-index:lib",
    "//crates/ctx-history-read-application:lib",
    "//crates/ctx-history-refresh:lib",
    "//crates/ctx-daemon-cli:lib",
    "//crates/ctx-client-observability:lib",
    "//crates/ctx-terminal:lib",
)
HISTORY_TEST_SUPPORT_DEPS = (
    "//crates/ctx-history-capture:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-platform:lib",
    "//crates/ctx-history-ingest-application:test_support_lib",
    "//crates/ctx-history-index:lib",
    "//crates/ctx-history-read-application:lib",
    "//crates/ctx-history-refresh:test_support_lib",
    "//crates/ctx-daemon-cli:test_support_lib",
    "//crates/ctx-client-observability:test_support_lib",
    "//crates/ctx-terminal:lib",
)
HISTORY_UNIT_TEST_DEPS = ("//crates/ctx-semantic-index:lib",)
HISTORY_LABEL = "//crates/ctx-history-cli:lib"
HISTORY_TEST_SUPPORT_LABEL = "//crates/ctx-history-cli:test_support_lib"
HISTORY_CARGO_DATA_LABEL = "//crates/ctx-history-cli:cargo_package_data"
HISTORY_BUILD_LABEL = "//crates/ctx-history-cli:BUILD.bazel"
HISTORY_CARGO_LABEL = "//crates/ctx-history-cli:Cargo.toml"
EVALUATED_REVERSE_BAZEL_CONSUMERS = {
    HISTORY_LABEL: (
        "//crates/ctx-cli-presentation:lib",
        "//crates/ctx-cli-presentation:qualification_lib",
        "//crates/ctx-cli:ctx",
        "//crates/ctx-cli:ctx_auto_upgrade_acceptance_fixture",
        "//crates/ctx-cli:ctx_hosted_uninstall_test_host",
        "//crates/ctx-cli:ctx_upgrade_test_harness",
        "//crates/ctx-history-cli:lib",
        "//crates/ctx-history-cli:request_parity_tests",
    ),
    HISTORY_TEST_SUPPORT_LABEL: (
        "//crates/ctx-cli-presentation:test_support_lib",
        "//crates/ctx-cli-presentation:unit_tests",
        "//crates/ctx-cli:unit_tests",
        "//crates/ctx-history-cli:test_support_lib",
    ),
}
DEPENDENCY_TABLES = {"dependencies", "dev-dependencies", "build-dependencies"}
HISTORY_LOADS = {
    "@crates//:defs.bzl": {"aliases", "all_crate_deps", "crate_edition"},
    "@rules_rust//rust:defs.bzl": {"rust_library"},
    "//:rust_sources.bzl": {"RUST_PROD_SRC_EXCLUDES"},
    "//tools/bazel:ctx_rust.bzl": {"ctx_rust_test"},
}
FINAL_LOADS = {
    "@crates//:defs.bzl": {"aliases", "all_crate_deps", "crate_deps", "crate_edition"},
    "@rules_rust//cargo:defs.bzl": {"cargo_toml_env_vars"},
    "//:rust_sources.bzl": {"RUST_PROD_SRC_EXCLUDES"},
    "//tools/bazel:ctx_rust.bzl": {"ctx_rust_binary", "ctx_rust_test"},
    ":test_targets.bzl": {
        "CTX_CLI_RUSTC_FLAGS",
        "ctx_cli_integration_test",
        "ctx_cli_test_data",
    },
}


class BoundaryError(ValueError):
    pass


@dataclass(frozen=True)
class Token:
    kind: str
    value: str


def _read_manifest(path: Path) -> dict[str, Any]:
    with path.open("rb") as handle:
        manifest = tomllib.load(handle)
    if not isinstance(manifest, dict):
        raise BoundaryError(f"{path} Cargo manifest must be a table")
    return manifest


def _workspace_dependencies(manifest: dict[str, Any]) -> dict[str, Any]:
    workspace = manifest.get("workspace")
    if not isinstance(workspace, dict):
        raise BoundaryError("root Cargo manifest must define a workspace table")
    dependencies = workspace.get("dependencies", {})
    if not isinstance(dependencies, dict):
        raise BoundaryError("root Cargo workspace.dependencies must be a table")
    return dependencies


def _workspace_members(manifest: dict[str, Any]) -> tuple[str, ...]:
    workspace = manifest.get("workspace")
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


def _dependency_tables(manifest: dict[str, Any], package: str) -> Iterator[tuple[str, dict[str, Any]]]:
    unsupported = sorted(
        name for name in manifest if name.endswith("dependencies") and name not in DEPENDENCY_TABLES
    )
    if unsupported:
        raise BoundaryError(f"{package} Cargo has unsupported dependency tables: {', '.join(unsupported)}")
    for name in DEPENDENCY_TABLES:
        table = manifest.get(name, {})
        if not isinstance(table, dict):
            raise BoundaryError(f"{package} Cargo {name} table must be a table")
        yield name, table
    targets = manifest.get("target", {})
    if not isinstance(targets, dict):
        raise BoundaryError(f"{package} Cargo target table must be a table")
    for target, tables in targets.items():
        if not isinstance(tables, dict):
            raise BoundaryError(f"{package} Cargo target {target!r} table must be a table")
        unknown = sorted(set(tables) - DEPENDENCY_TABLES)
        if unknown:
            raise BoundaryError(f"{package} Cargo target {target!r} has unsupported tables: {', '.join(unknown)}")
        for name, table in tables.items():
            if not isinstance(table, dict):
                raise BoundaryError(f"{package} Cargo target {target!r} {name} table must be a table")
            yield f"target.{target}.{name}", table


def _canonical_dependency_name(key: str, value: Any, package: str, table: str, workspace: dict[str, Any], *, workspace_entry: bool = False) -> str:
    context = f"{package} Cargo {table} dependency {key!r}"
    if not isinstance(key, str) or not key:
        raise BoundaryError(f"{context} has an invalid dependency key")
    if isinstance(value, str):
        return key
    if not isinstance(value, dict):
        raise BoundaryError(f"{context} must be a string or inline table")
    renamed = value.get("package")
    if renamed is not None and (not isinstance(renamed, str) or not renamed):
        raise BoundaryError(f"{context} has an invalid package rename")
    inherited = value.get("workspace")
    if inherited is not None and not isinstance(inherited, bool):
        raise BoundaryError(f"{context} has a non-boolean workspace inheritance flag")
    if workspace_entry and inherited is not None:
        raise BoundaryError(f"{context} cannot inherit from workspace.dependencies")
    if inherited is False:
        raise BoundaryError(f"{context} has an ambiguous workspace = false entry")
    if inherited is True:
        if renamed is not None:
            raise BoundaryError(f"{context} cannot combine workspace inheritance with a package rename")
        workspace_value = workspace.get(key)
        if workspace_value is None:
            raise BoundaryError(f"{context} is absent from root workspace.dependencies")
        return _canonical_dependency_name(key, workspace_value, "root workspace", "dependencies", workspace, workspace_entry=True)
    return renamed or key


def _resolved_dependencies(manifest: dict[str, Any], package: str, workspace: dict[str, Any]) -> list[tuple[str, str]]:
    resolved: list[tuple[str, str]] = []
    for table_name, table in _dependency_tables(manifest, package):
        for key, value in table.items():
            resolved.append((table_name, _canonical_dependency_name(key, value, package, table_name, workspace)))
    return resolved


def _package_name(manifest: dict[str, Any], path: Path) -> str:
    package = manifest.get("package")
    if not isinstance(package, dict) or not isinstance(package.get("name"), str) or not package["name"]:
        raise BoundaryError(f"{path} Cargo package.name must be a non-empty string")
    return package["name"]


def _validate_cargo(workspace_path: Path, history_path: Path, final_path: Path, member_paths: Sequence[Path]) -> None:
    workspace_manifest = _read_manifest(workspace_path)
    workspace = _workspace_dependencies(workspace_manifest)
    history_manifest = _read_manifest(history_path)
    final_manifest = _read_manifest(final_path)
    if _package_name(history_manifest, history_path) != HISTORY_PACKAGE:
        raise BoundaryError("history Cargo package identity drifted")
    if _package_name(final_manifest, final_path) != FINAL_PACKAGE:
        raise BoundaryError("final Cargo package identity drifted")
    resolved_history_dependencies = _resolved_dependencies(history_manifest, HISTORY_PACKAGE, workspace)
    history_dependencies = {name for _, name in resolved_history_dependencies}
    forbidden = sorted(name for name in history_dependencies if name in FORBIDDEN_HISTORY_CARGO or name.startswith("ctx-agent-"))
    if forbidden:
        raise BoundaryError("ctx-history-cli has forbidden Cargo dependencies: " + ", ".join(forbidden))
    # Cargo dev scope is package-wide; the exact unit target is checked in Bazel.
    # Resolve renames and workspace inheritance before checking every table.
    semantic_scopes = [table for table, name in resolved_history_dependencies if name == "ctx-semantic-index"]
    if semantic_scopes != ["dev-dependencies"]:
        raise BoundaryError("ctx-history-cli Cargo ctx-semantic-index must appear exactly once in dev-dependencies")

    members = _workspace_members(workspace_manifest)
    expected = {(workspace_path.parent / member / "Cargo.toml").resolve() for member in members}
    supplied = {path.resolve() for path in member_paths}
    if expected != supplied or len(supplied) != len(member_paths):
        raise BoundaryError("Cargo member manifest inputs drifted")
    reverse: list[tuple[str, str]] = []
    for path in member_paths:
        manifest = _read_manifest(path)
        consumer = _package_name(manifest, path)
        for table, dependency in _resolved_dependencies(manifest, consumer, workspace):
            if dependency == HISTORY_PACKAGE:
                reverse.append((consumer, table))
    if sorted(reverse) != [
        (FINAL_PACKAGE, "dependencies"),
        ("ctx-cli-presentation", "dependencies"),
    ]:
        raise BoundaryError(f"ctx-history-cli reverse Cargo consumers drifted: {sorted(reverse)}")


def _tokenize(source: str, package: str) -> list[Token]:
    tokens: list[Token] = []
    index = 0
    while index < len(source):
        character = source[index]
        if character.isspace():
            index += 1
        elif character == "#":
            newline = source.find("\n", index)
            index = len(source) if newline == -1 else newline + 1
        elif character in {"'", '"'}:
            start, quote = index, character
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
        elif character.isalpha() or character == "_":
            start = index
            index += 1
            while index < len(source) and (source[index].isalnum() or source[index] == "_"):
                index += 1
            tokens.append(Token("identifier", source[start:index]))
        else:
            tokens.append(Token("symbol", character))
            index += 1
    return tokens


def _split(tokens: Sequence[Token]) -> list[list[Token]]:
    parts: list[list[Token]] = [[]]
    depth = 0
    pairs = {"(": ")", "[": "]", "{": "}"}
    for token in tokens:
        if token.value in pairs:
            depth += 1
        elif token.value in set(pairs.values()):
            depth -= 1
            if depth < 0:
                raise BoundaryError("Bazel has unbalanced delimiters")
        if token.value == "," and depth == 0:
            parts.append([])
        else:
            parts[-1].append(token)
    if depth:
        raise BoundaryError("Bazel has unbalanced delimiters")
    return [part for part in parts if part]


def _calls(tokens: Sequence[Token], name: str, package: str) -> list[list[Token]]:
    calls: list[list[Token]] = []
    for index, token in enumerate(tokens[:-1]):
        if token.kind != "identifier" or token.value != name or tokens[index + 1].value != "(":
            continue
        depth = 0
        for end in range(index + 1, len(tokens)):
            if tokens[end].value == "(":
                depth += 1
            elif tokens[end].value == ")":
                depth -= 1
                if depth == 0:
                    calls.append(list(tokens[index + 2:end]))
                    break
        else:
            raise BoundaryError(f"{package} Bazel has an unterminated {name} call")
    return calls


def _named(call: Sequence[Token], package: str) -> dict[str, list[Token]]:
    arguments: dict[str, list[Token]] = {}
    for part in _split(call):
        if len(part) < 3 or part[0].kind != "identifier" or part[1].value != "=":
            raise BoundaryError(f"{package} Bazel target has an unsupported argument")
        if part[0].value in arguments:
            raise BoundaryError(f"{package} Bazel target repeats {part[0].value}")
        arguments[part[0].value] = part[2:]
    return arguments


def _literal_list(tokens: Sequence[Token], package: str, context: str) -> tuple[str, ...]:
    if len(tokens) < 2 or tokens[0].value != "[" or tokens[-1].value != "]":
        raise BoundaryError(f"{package} Bazel {context} must be a literal string list")
    values: list[str] = []
    for item in _split(tokens[1:-1]):
        if len(item) != 1 or item[0].kind != "string":
            raise BoundaryError(f"{package} Bazel {context} must contain only literal string labels")
        values.append(item[0].value)
    if len(values) != len(set(values)):
        raise BoundaryError(f"{package} Bazel {context} has duplicate labels")
    return tuple(values)


def _assignment(tokens: Sequence[Token], name: str, package: str) -> list[list[Token]]:
    values: list[list[Token]] = []
    for index, token in enumerate(tokens):
        if token.kind != "identifier" or token.value != name:
            continue
        if index + 1 >= len(tokens) or tokens[index + 1].value != "=":
            continue
        value = list(tokens[index + 2:])
        if not value or value[0].value != "[":
            values.append(value[:1])
            continue
        depth = 0
        for end, value_token in enumerate(value):
            if value_token.value == "[":
                depth += 1
            elif value_token.value == "]":
                depth -= 1
                if depth == 0:
                    following = value[end + 1:]
                    # Do not authenticate only the first list of a composed
                    # assignment. A literal must end before the next statement.
                    if following and not (
                        len(following) >= 2
                        and following[0].kind == "identifier"
                        and following[1].value in {"=", "("}
                    ):
                        raise BoundaryError(f"{package} Bazel {name} inventory drifted: must be a standalone literal string list")
                    values.append(value[:end + 1])
                    break
        else:
            raise BoundaryError(f"{package} Bazel {name} assignment has unbalanced delimiters")
    return values


def _validate_loads(tokens: Sequence[Token], package: str, expected: dict[str, set[str]], reserved: set[str]) -> None:
    actual: dict[str, set[str]] = {}
    canonical_symbols = {symbol: source for source, symbols in expected.items() for symbol in symbols}
    for call in _calls(tokens, "load", package):
        arguments = _split(call)
        if not arguments or len(arguments[0]) != 1 or arguments[0][0].kind != "string":
            raise BoundaryError(f"{package} Bazel has an unsupported load source")
        source = arguments[0][0].value
        if source in actual or source not in expected:
            raise BoundaryError(f"{package} Bazel has an unsupported or duplicate load source: {source}")
        imported: list[str] = []
        for argument in arguments[1:]:
            if len(argument) != 1 or argument[0].kind != "string":
                raise BoundaryError(f"{package} Bazel load aliases or custom bindings are unsupported")
            symbol = argument[0].value
            if symbol in reserved:
                raise BoundaryError(f"{package} Bazel {symbol} must be a local literal and may not be loaded")
            if symbol in canonical_symbols and canonical_symbols[symbol] != source:
                raise BoundaryError(f"{package} Bazel trusted symbol {symbol} has a noncanonical load source")
            imported.append(symbol)
        if len(imported) != len(set(imported)) or set(imported) != expected[source]:
            raise BoundaryError(f"{package} Bazel {source} load bindings drifted")
        actual[source] = set(imported)
    if set(actual) != set(expected):
        raise BoundaryError(f"{package} Bazel canonical loads drifted")


def _validate_call_surface(tokens: Sequence[Token], package: str, allowed: set[str]) -> None:
    for index, token in enumerate(tokens):
        if token.value != "(":
            continue
        caller = tokens[index - 1] if index else None
        if caller is None or caller.kind != "identifier" or caller.value not in allowed:
            name = caller.value if caller else "<expression>"
            raise BoundaryError(f"{package} Bazel has an unsupported rule or macro call: {name}")
    for index, token in enumerate(tokens):
        if token.kind != "identifier" or token.value not in allowed:
            continue
        previous = tokens[index - 1].value if index else None
        following = tokens[index + 1].value if index + 1 < len(tokens) else None
        if token.value == "aliases" and following == "=" and previous in {"(", ","}:
            continue
        if following != "(" or previous in {".", "def"}:
            raise BoundaryError(f"{package} Bazel rule or macro symbol {token.value} may not be rebound or aliased")


def _all_crate_deps(tokens: Sequence[Token], expected: dict[str, str]) -> bool:
    if len(tokens) < 3 or tokens[0].value != "all_crate_deps" or tokens[1].value != "(" or tokens[-1].value != ")":
        return False
    actual: dict[str, str] = {}
    for argument in _split(tokens[2:-1]):
        if len(argument) != 3 or argument[0].kind != "identifier" or argument[1].value != "=" or argument[2].kind != "identifier" or argument[0].value in actual:
            return False
        actual[argument[0].value] = argument[2].value
    return actual == expected


def _dependency_expression(tokens: Sequence[Token], package: str, context: str, flags: dict[str, str], variables: tuple[str, ...]) -> None:
    plus = [index for index, token in enumerate(tokens) if token.value == "+"]
    expected_suffix = list(variables)
    valid = len(plus) == len(expected_suffix) and _all_crate_deps(tokens[:plus[0]] if plus else tokens, flags)
    if valid:
        valid = [token.value for index in plus for token in tokens[index + 1: index + 2]] == expected_suffix
        if valid:
            positions = plus + [len(tokens)]
            valid = all(positions[index] + 2 == positions[index + 1] for index in range(len(plus)))
    if not valid:
        rendered = " + ".join(("all_crate_deps(...)", *variables))
        raise BoundaryError(f"{package} Bazel {context} must be exactly {rendered}")


def _validate_rule(call: Sequence[Token], package: str, context: str, deps_flags: dict[str, str], proc_flags: dict[str, str], variables: tuple[str, ...]) -> str:
    arguments = _named(call, package)
    name = arguments.get("name")
    if name is None or len(name) != 1 or name[0].kind != "string":
        raise BoundaryError(f"{package} Bazel {context} must have a literal name")
    _dependency_expression(arguments.get("deps", []), package, f"{context} deps", deps_flags, variables)
    _dependency_expression(arguments.get("proc_macro_deps", []), package, f"{context} proc_macro_deps", proc_flags, ())
    return name[0].value


def _rule_name(call: Sequence[Token], package: str, context: str) -> str:
    name = _named(call, package).get("name")
    if name is None or len(name) != 1 or name[0].kind != "string":
        raise BoundaryError(f"{package} Bazel {context} must have a literal name")
    return name[0].value


def _validate_history_build(path: Path) -> None:
    package = HISTORY_PACKAGE
    tokens = _tokenize(path.read_text(encoding="utf-8"), package)
    _validate_loads(tokens, package, HISTORY_LOADS, {"HISTORY_CLI_DEPS", "HISTORY_CLI_TEST_SUPPORT_DEPS", "HISTORY_CLI_UNIT_TEST_DEPS"})
    _validate_call_surface(tokens, package, {"aliases", "all_crate_deps", "crate_edition", "ctx_rust_test", "exports_files", "filegroup", "glob", "load", "package", "rust_library"})
    for variable, expected, expected_uses in (
        ("HISTORY_CLI_DEPS", HISTORY_DEPS, 4),
        ("HISTORY_CLI_TEST_SUPPORT_DEPS", HISTORY_TEST_SUPPORT_DEPS, 2),
        ("HISTORY_CLI_UNIT_TEST_DEPS", HISTORY_UNIT_TEST_DEPS, 2),
    ):
        values = _assignment(tokens, variable, package)
        if len(values) != 1 or _literal_list(values[0], package, variable) != expected:
            raise BoundaryError(f"{package} Bazel {variable} inventory drifted")
        if sum(token.kind == "identifier" and token.value == variable for token in tokens) != expected_uses:
            raise BoundaryError(f"{package} Bazel {variable} may only be assigned and used by its reviewed targets")
    libraries = _calls(tokens, "rust_library", package)
    if len(libraries) != 2:
        raise BoundaryError(f"{package} Bazel must define exactly two rust_library targets")
    # Validate each exact shape without accepting a swapped dependency inventory.
    seen_libraries: set[str] = set()
    for call in libraries:
        name = _rule_name(call, package, "rust_library")
        arguments = _named(call, package)
        if name == "test_support_lib":
            if [token.value for token in arguments.get("testonly", [])] != ["True"]:
                raise BoundaryError(
                    f"{package} Bazel test_support_lib must set testonly = True"
                )
            dependencies = ("HISTORY_CLI_TEST_SUPPORT_DEPS",)
        else:
            if "testonly" in arguments:
                raise BoundaryError(
                    f"{package} Bazel production lib must not be testonly"
                )
            dependencies = ("HISTORY_CLI_DEPS",)
        seen_libraries.add(_validate_rule(call, package, "rust_library", {"normal": "True"}, {"proc_macro": "True"}, dependencies))
    if seen_libraries != {"lib", "test_support_lib"}:
        raise BoundaryError(f"{package} Bazel rust_library names drifted")
    tests = _calls(tokens, "ctx_rust_test", package)
    if len(tests) != 2:
        raise BoundaryError(f"{package} Bazel must define exactly two ctx_rust_test targets")
    expected_tests = {
        "unit_tests": ("HISTORY_CLI_DEPS", "HISTORY_CLI_UNIT_TEST_DEPS"),
        "request_parity_tests": ("HISTORY_CLI_DEPS", "[SELF]"),
    }
    seen_tests: set[str] = set()
    for call in tests:
        name = _rule_name(call, package, "ctx_rust_test")
        if name == "request_parity_tests":
            arguments = _named(call, package)
            expected = ("HISTORY_CLI_DEPS",)
            dependencies = arguments.get("deps", [])
            if [token.value for token in dependencies[-5:]] != ["+", "[", HISTORY_LABEL, ",", "]"]:
                raise BoundaryError(f"{package} Bazel request_parity_tests self edge drifted")
            _dependency_expression(dependencies[:-5], package, "request_parity_tests deps", {"normal": "True", "normal_dev": "True"}, expected)
            _dependency_expression(arguments.get("proc_macro_deps", []), package, "request_parity_tests proc_macro_deps", {"proc_macro": "True", "proc_macro_dev": "True"}, ())
        else:
            _validate_rule(call, package, "ctx_rust_test", {"normal": "True", "normal_dev": "True"}, {"proc_macro": "True", "proc_macro_dev": "True"}, expected_tests["unit_tests"])
        seen_tests.add(name)
    if set(expected_tests) != seen_tests:
        raise BoundaryError(f"{package} Bazel ctx_rust_test names drifted")


def _validate_final_build(path: Path) -> None:
    package = "ctx-cli"
    tokens = _tokenize(path.read_text(encoding="utf-8"), package)
    _validate_loads(tokens, package, FINAL_LOADS, {"CTX_CLI_DEPS", "CTX_CLI_TEST_DEPS", "CTX_CLI_QUALIFICATION_DEPS"})
    _validate_call_surface(tokens, package, {"aliases", "all_crate_deps", "cargo_toml_env_vars", "crate_deps", "crate_edition", "ctx_cli_integration_test", "ctx_cli_test_data", "ctx_rust_binary", "ctx_rust_test", "dict", "exports_files", "filegroup", "glob", "load", "package", "select", "test_suite"})
    expected_labels = {
        "CTX_CLI_DEPS": (HISTORY_LABEL, 2),
        "CTX_CLI_TEST_DEPS": (HISTORY_TEST_SUPPORT_LABEL, 2),
        "CTX_CLI_QUALIFICATION_DEPS": (HISTORY_LABEL, 4),
    }
    for variable, (label, expected_uses) in expected_labels.items():
        values = _assignment(tokens, variable, package)
        if len(values) != 1:
            raise BoundaryError(f"{package} Bazel {variable} must be assigned exactly once")
        labels = _literal_list(values[0], package, variable)
        if labels.count(label) != 1:
            raise BoundaryError(f"{package} Bazel {variable} reverse history-cli edge drifted")
        actual_uses = sum(
            token.kind == "identifier" and token.value == variable
            for token in tokens
        )
        if actual_uses != expected_uses:
            raise BoundaryError(
                f"{package} Bazel {variable} may only be assigned and used by "
                "its reviewed Rust targets"
            )
    history_strings = [token.value for token in tokens if token.kind == "string" and "ctx-history-cli" in token.value]
    if sorted(history_strings) != sorted([HISTORY_LABEL, HISTORY_LABEL, HISTORY_TEST_SUPPORT_LABEL]):
        raise BoundaryError(f"{package} Bazel reverse history-cli labels drifted")
    binary_expected = {
        "ctx": ("CTX_CLI_DEPS",),
        "ctx_auto_upgrade_acceptance_fixture": ("CTX_CLI_QUALIFICATION_DEPS",),
        "ctx_hosted_uninstall_test_host": ("CTX_CLI_QUALIFICATION_DEPS",),
        "ctx_upgrade_test_harness": ("CTX_CLI_QUALIFICATION_DEPS",),
    }
    seen: set[str] = set()
    for call in _calls(tokens, "ctx_rust_binary", package):
        name = _rule_name(call, package, "ctx_rust_binary")
        variables = binary_expected.get(name)
        if variables is None:
            raise BoundaryError(f"{package} Bazel unexpected Rust binary {name}")
        _validate_rule(call, package, f"ctx_rust_binary {name}", {"normal": "True"}, {"proc_macro": "True"}, variables)
        seen.add(name)
    if seen != set(binary_expected):
        raise BoundaryError(f"{package} Bazel Rust binary inventory drifted")
    tests = _calls(tokens, "ctx_rust_test", package)
    if len(tests) != 1 or _validate_rule(tests[0], package, "ctx_rust_test", {"normal": "True", "normal_dev": "True"}, {"proc_macro": "True", "proc_macro_dev": "True"}, ("CTX_CLI_TEST_DEPS",)) != "unit_tests":
        raise BoundaryError(f"{package} Bazel Rust test inventory drifted")


def _validate_reverse_build_inventory(
    workspace_path: Path,
    history_build: Path,
    final_build: Path,
    build_paths: Sequence[Path],
) -> None:
    workspace_manifest = _read_manifest(workspace_path)
    expected_paths = {
        workspace_path.parent.resolve() / "BUILD.bazel",
        *(
            workspace_path.parent.resolve() / member / "BUILD.bazel"
            for member in _workspace_members(workspace_manifest)
        ),
    }
    supplied_paths = {path.resolve() for path in build_paths}
    if supplied_paths != expected_paths or len(supplied_paths) != len(build_paths):
        raise BoundaryError("Cargo workspace BUILD input inventory drifted")

    root_build = workspace_path.parent.resolve() / "BUILD.bazel"
    terminal_build = workspace_path.parent.resolve() / "crates/ctx-terminal/BUILD.bazel"
    presentation_build = workspace_path.parent.resolve() / "crates/ctx-cli-presentation/BUILD.bazel"
    expected_labels = {
        # Release-package audit inputs now live in their loaded .bzl; this
        # inventory covers only labels written directly in the root BUILD.
        root_build: (
            HISTORY_CARGO_DATA_LABEL,
            HISTORY_BUILD_LABEL,
            HISTORY_BUILD_LABEL,
            HISTORY_CARGO_DATA_LABEL,
        ),
        history_build.resolve(): (HISTORY_LABEL,),
        final_build.resolve(): (
            HISTORY_LABEL,
            HISTORY_TEST_SUPPORT_LABEL,
            HISTORY_LABEL,
        ),
        presentation_build: (HISTORY_LABEL, HISTORY_TEST_SUPPORT_LABEL, HISTORY_LABEL),
        terminal_build: (HISTORY_CARGO_DATA_LABEL,),
    }
    for path in build_paths:
        tokens = _tokenize(path.read_text(encoding="utf-8"), str(path))
        actual = tuple(
            token.value
            for token in tokens
            if token.kind == "string"
            and token.value.startswith("//crates/ctx-history-cli:")
        )
        expected = expected_labels.get(path.resolve(), ())
        if sorted(actual) != sorted(expected):
            raise BoundaryError(
                f"unexpected reverse ctx-history-cli Bazel consumer in {path}: "
                f"expected={sorted(expected)} actual={sorted(actual)}"
            )


def validate_evaluated_reverse_bazel_consumers(
    query: Callable[[str], Sequence[str]],
) -> None:
    """Require the exact direct Bazel consumers after Starlark evaluation.

    The lexical BUILD inventory above deliberately constrains the reviewed
    files. This check is separate because labels may also be composed or
    provided by a loaded macro, neither of which can be safely inferred from
    raw string tokens.
    """
    for target, expected in EVALUATED_REVERSE_BAZEL_CONSUMERS.items():
        actual = tuple(sorted(query(f"rdeps(//..., {target}, 1)")))
        if actual != expected:
            raise BoundaryError(
                "ctx-history-cli evaluated reverse Bazel consumers drifted: "
                f"target={target} expected={list(expected)} actual={list(actual)}"
            )


def _validate_live_reverse_bazel_consumers(workspace_path: Path) -> None:
    repo_root = workspace_path.parent.resolve()
    bazel_wrapper = repo_root / "scripts/bazelw"
    if not bazel_wrapper.is_file() or not os.access(bazel_wrapper, os.X_OK):
        raise BoundaryError("history CLI boundary requires an executable scripts/bazelw")

    # Bazel may set TEST_TMPDIR beneath the checkout. Its repository cache must
    # remain outside the workspace, so do not let tempfile inherit that path.
    with tempfile.TemporaryDirectory(
        prefix="ctx-history-cli-boundary-", dir="/tmp"
    ) as scratch:
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
            if result.returncode:
                detail = result.stderr.strip() or result.stdout.strip()
                raise BoundaryError(
                    "history CLI evaluated reverse Bazel query failed"
                    + (f": {detail}" if detail else "")
                )
            return tuple(line for line in result.stdout.splitlines() if line)

        validate_evaluated_reverse_bazel_consumers(query)


def _raw_rust_string_end(source: str, index: int) -> int | None:
    marker = index
    if source.startswith(("br", "cr"), marker):
        marker += 1
    if marker >= len(source) or source[marker] != "r":
        return None
    marker += 1
    hashes = 0
    while marker < len(source) and source[marker] == "#":
        hashes += 1
        marker += 1
    if marker >= len(source) or source[marker] != '"':
        return None
    terminator = '"' + ("#" * hashes)
    end = source.find(terminator, marker + 1)
    if end < 0:
        raise BoundaryError("Rust source has an unterminated raw string")
    return end + len(terminator)


def _rust_tokens(source: str) -> list[Token]:
    """Tokenize identifiers and paths while honoring Rust comments and strings."""
    tokens: list[Token] = []
    index = 0
    while index < len(source):
        if source.startswith("//", index):
            newline = source.find("\n", index + 2)
            index = len(source) if newline < 0 else newline + 1
            continue
        if source.startswith("/*", index):
            depth = 1
            index += 2
            while index < len(source) and depth:
                if source.startswith("/*", index):
                    depth += 1
                    index += 2
                elif source.startswith("*/", index):
                    depth -= 1
                    index += 2
                else:
                    index += 1
            if depth:
                raise BoundaryError("Rust source has an unterminated block comment")
            continue
        raw_end = _raw_rust_string_end(source, index)
        if raw_end is not None:
            tokens.append(Token("literal", "<raw-string>"))
            index = raw_end
            continue
        if source[index] == '"':
            index += 1
            escaped = False
            while index < len(source):
                character = source[index]
                index += 1
                if escaped:
                    escaped = False
                elif character == "\\":
                    escaped = True
                elif character == '"':
                    break
            else:
                raise BoundaryError("Rust source has an unterminated string")
            tokens.append(Token("literal", "<string>"))
            continue
        if source[index].isalpha() or source[index] == "_":
            start = index
            index += 1
            while index < len(source) and (
                source[index].isalnum() or source[index] == "_"
            ):
                index += 1
            tokens.append(Token("identifier", source[start:index]))
            continue
        if not source[index].isspace():
            tokens.append(Token("symbol", source[index]))
        index += 1
    return tokens


def _rust_identifiers(source: str) -> list[str]:
    return [
        token.value for token in _rust_tokens(source) if token.kind == "identifier"
    ]


def _qualified_rust_paths(tokens: Sequence[Token]) -> Iterator[tuple[str, str]]:
    for index in range(len(tokens) - 3):
        path = tokens[index : index + 4]
        if (
            path[0].kind == "identifier"
            and path[1].value == ":"
            and path[2].value == ":"
            and path[3].kind == "identifier"
        ):
            yield path[0].value, path[3].value


def _validate_rust_sources(history_source_root: Path, provider_args: Path, provider_sources: Path) -> None:
    for path in sorted(history_source_root.rglob("*.rs")):
        tokens = _rust_tokens(path.read_text(encoding="utf-8"))
        forbidden = {
            f"{namespace}::{member}"
            for namespace, member in _qualified_rust_paths(tokens)
            if namespace in {"clap", "ctx_cli"}
            or namespace.startswith("ctx_agent_")
            or (namespace == "identity" and member == "home_dir")
            or (namespace == "CaptureProvider" and member == "Unknown")
        }
        if forbidden:
            raise BoundaryError(f"ctx-history-cli Rust source has prohibited identifiers in {path.name}: {', '.join(sorted(forbidden))}")
    provider_arg_tokens = _rust_tokens(provider_args.read_text(encoding="utf-8"))
    for namespace, member in _qualified_rust_paths(provider_arg_tokens):
        if namespace == "CaptureProvider" and member[:1].isupper():
            raise BoundaryError("provider vocabulary must not be duplicated in the final Clap/value-parser shell")
    provider_source_words = _rust_identifiers(provider_sources.read_text(encoding="utf-8"))
    if "discover_provider_sources" in provider_source_words or "discover_provider_sources_for_provider_report" in provider_source_words or "discover_provider_sources_report" in provider_source_words:
        raise BoundaryError("native discovery must be owned by ctx-history-cli, not its final compatibility wrapper")


def validate(
    workspace: Path,
    history_cargo: Path,
    history_build: Path,
    final_cargo: Path,
    final_build: Path,
    history_source_root: Path,
    provider_args: Path,
    provider_sources: Path,
    member_cargos: Sequence[Path],
    member_builds: Sequence[Path],
) -> None:
    _validate_cargo(workspace, history_cargo, final_cargo, member_cargos)
    _validate_history_build(history_build)
    _validate_final_build(final_build)
    _validate_reverse_build_inventory(
        workspace, history_build, final_build, member_builds
    )
    _validate_rust_sources(history_source_root, provider_args, provider_sources)


def main() -> int:
    if sys.argv.count("--member-builds") != 1:
        print(
            "usage: check_history_cli_boundary.py WORKSPACE_CARGO HISTORY_CARGO "
            "HISTORY_BUILD FINAL_CARGO FINAL_BUILD HISTORY_SRC PROVIDER_ARGS "
            "PROVIDER_SOURCES MEMBER_CARGO... --member-builds BUILD...",
            file=sys.stderr,
        )
        return 64
    build_separator = sys.argv.index("--member-builds")
    if build_separator < 10 or build_separator == len(sys.argv) - 1:
        print("history CLI boundary requires Cargo and BUILD inventories", file=sys.stderr)
        return 64
    try:
        validate(
            *(Path(argument) for argument in sys.argv[1:9]),
            tuple(Path(argument) for argument in sys.argv[9:build_separator]),
            tuple(Path(argument) for argument in sys.argv[build_separator + 1 :]),
        )
        _validate_live_reverse_bazel_consumers(Path(sys.argv[1]))
    except (BoundaryError, OSError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    print("ctx-history-cli static Cargo, Bazel, and source boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
