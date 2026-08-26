#!/usr/bin/env python3
"""Static dependency boundary for history runtime, JSONL, and native JSONL."""

from __future__ import annotations

import ast
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterator, Sequence


EXPECTED_RUNTIME_DEPENDENCIES = {
    "ctx-history-capture-model": {"path": "../ctx-history-capture-model"},
    "ctx-history-core": {"path": "../ctx-history-core"},
    "thiserror": {"workspace": True},
    "uuid": {"workspace": True},
}
EXPECTED_RUNTIME_DEV_DEPENDENCIES: dict[str, dict[str, object]] = {}
RUNTIME_FORBIDDEN_CARGO = {
    "ctx-history-capture",
    "ctx-history-index",
    "ctx-history-index-format",
    "ctx-history-jsonl",
}
RUNTIME_DIRECT_BAZEL_DEPENDENCIES = (
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-core:lib",
)
JSONL_FORBIDDEN_CARGO = {
    "ctx-history-capture",
    "ctx-history-index",
    "ctx-history-index-format",
    "ctx-history-index-generation",
    "ctx-history-index-query",
    "ctx-history-source-sqlite",
}
JSONL_DIRECT_BAZEL_DEPENDENCIES = (
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-capture-runtime:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-source-io:lib",
)
JSONL_TEST_DIRECT_BAZEL_DEPENDENCIES = (
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-capture-runtime:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-source-io:test_support_lib",
)
EXPECTED_PROVIDER_DEPENDENCIES = {
    "chrono": {"workspace": True},
    "ctx-history-capture-model": {"path": "../ctx-history-capture-model"},
    "ctx-history-capture-runtime": {"path": "../ctx-history-capture-runtime"},
    "ctx-history-core": {"path": "../ctx-history-core"},
    "ctx-history-jsonl": {"path": "../ctx-history-jsonl"},
    "ctx-history-native-jsonl-parsers": {
        "path": "../ctx-history-native-jsonl-parsers"
    },
    "ctx-history-source-io": {"path": "../ctx-history-source-io"},
    "serde": {"workspace": True},
    "serde_json": {"workspace": True},
    "thiserror": {"workspace": True},
}
EXPECTED_PROVIDER_DEV_DEPENDENCIES = {
    "ctx-history-jsonl": {
        "path": "../ctx-history-jsonl",
        "features": ["test-support"],
    },
    "ctx-history-source-io": {
        "path": "../ctx-history-source-io",
        "features": ["test-support"],
    },
    "tempfile": {"workspace": True},
    "uuid": {"workspace": True},
}
PROVIDER_FORBIDDEN_CARGO = {
    "ctx-history-capture",
    "ctx-history-index",
    "ctx-history-index-format",
    "ctx-history-index-generation",
    "ctx-history-index-query",
}
PROVIDER_DIRECT_BAZEL_DEPENDENCIES = (
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-capture-runtime:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-jsonl:lib",
    "//crates/ctx-history-native-jsonl-parsers:lib",
    "//crates/ctx-history-source-io:lib",
)
PROVIDER_TEST_DIRECT_BAZEL_DEPENDENCIES = (
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-capture-runtime:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-jsonl:test_support_lib",
    "//crates/ctx-history-native-jsonl-parsers:lib",
    "//crates/ctx-history-source-io:test_support_lib",
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
            raise BoundaryError(
                f"{package} Cargo {table_name} table must be a table"
            )
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


def _validate_no_forbidden_cargo_dependencies(
    manifest: dict[str, Any],
    package: str,
    forbidden: set[str],
    workspace_manifest: dict[str, Any],
) -> None:
    forbidden_dependencies = sorted(
        _canonical_dependency_names(manifest, package, workspace_manifest) & forbidden
    )
    if forbidden_dependencies:
        raise BoundaryError(
            f"{package} has forbidden Cargo dependencies: "
            + ", ".join(forbidden_dependencies)
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


def _find_calls(
    tokens: Sequence[Token], rule: str, package: str
) -> list[list[Token]]:
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
    items = _split_top_level(tokens[1:-1])
    values: list[str] = []
    for item in items:
        if len(item) != 1 or item[0].kind != "string":
            raise BoundaryError(f"{package} Bazel {name} must contain only string labels")
        values.append(item[0].value)
    if len(values) != len(set(values)):
        raise BoundaryError(f"{package} Bazel {name} has duplicate labels")
    return tuple(values)


def _assignments(
    tokens: Sequence[Token], name: str
) -> list[tuple[str, list[Token]]]:
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
            raise BoundaryError(
                f"{package} Bazel must load {source} exactly once"
            )
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
                    f"{package} Bazel {remote_name} must be a local literal and "
                    "may not be loaded"
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
            raise BoundaryError(
                f"{package} Bazel has an unsupported load source: {source}"
            )
        if len(imported_names) != len(set(imported_names)):
            raise BoundaryError(f"{package} Bazel {source} repeats a load binding")
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
            f"{package} Bazel is missing canonical loads: "
            + ", ".join(missing_sources)
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
                f"{package} Bazel has an unsupported rule or macro call: "
                f"{name}"
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


def _validate_dependency_expression(
    tokens: Sequence[Token],
    package: str,
    context: str,
    generated_flags: dict[str, str],
    direct_dependencies: str | None = None,
) -> None:
    if direct_dependencies is None:
        valid = _is_all_crate_deps(tokens, **generated_flags)
    else:
        plus = [index for index, token in enumerate(tokens) if token.value == "+"]
        valid = (
            len(plus) == 1
            and _is_all_crate_deps(tokens[: plus[0]], **generated_flags)
            and [token.value for token in tokens[plus[0] + 1 :]]
            == [direct_dependencies]
        )
    if not valid:
        suffix = f" + {direct_dependencies}" if direct_dependencies else ""
        rendered_flags = ", ".join(
            f"{name} = {value}" for name, value in generated_flags.items()
        )
        raise BoundaryError(
            f"{package} Bazel {context} must be exactly "
            f"all_crate_deps({rendered_flags}){suffix}"
        )


def _validate_rule_dependencies(
    call: Sequence[Token],
    package: str,
    context: str,
    dependency_flags: dict[str, str],
    proc_macro_flags: dict[str, str],
    direct_dependencies: str | None = None,
) -> dict[str, list[Token]]:
    arguments = _named_arguments(call, package)
    name = arguments.get("name")
    if name is None or len(name) != 1 or name[0].kind != "string":
        raise BoundaryError(f"{package} Bazel {context} must have a literal name")
    _validate_dependency_expression(
        arguments.get("deps", []),
        package,
        f"{context} deps",
        dependency_flags,
        direct_dependencies,
    )
    _validate_dependency_expression(
        arguments.get("proc_macro_deps", []),
        package,
        f"{context} proc_macro_deps",
        proc_macro_flags,
    )
    return arguments


def _validate_package_bazel(build_path: Path, package: str, *, jsonl: bool) -> None:
    tokens = _tokenize_starlark(build_path.read_text(encoding="utf-8"), package)
    dependency_variables = (
        {"JSONL_DEPS", "JSONL_TEST_DEPS"} if jsonl else {"RUNTIME_DEPS"}
    )
    _validate_canonical_loads(
        tokens, package, dependency_variables
    )
    _validate_call_surface(tokens, package)

    rust_libraries = _find_calls(tokens, "rust_library", package)
    expected_library_count = 2 if jsonl else 1
    if len(rust_libraries) != expected_library_count:
        raise BoundaryError(
            f"{package} Bazel must define exactly {expected_library_count} "
            "rust_library target(s)"
        )
    direct_dependencies = "JSONL_DEPS" if jsonl else "RUNTIME_DEPS"
    library_arguments = _validate_rule_dependencies(
        rust_libraries[0],
        package,
        "rust_library",
        {"normal": "True"},
        {"proc_macro": "True"},
        direct_dependencies,
    )
    if library_arguments["name"][0].value != "lib":
        raise BoundaryError(f"{package} Bazel rust_library must be named lib")

    if jsonl:
        test_support_arguments = _validate_rule_dependencies(
            rust_libraries[1],
            package,
            "test-support rust_library",
            {"normal": "True"},
            {"proc_macro": "True"},
            "JSONL_TEST_DEPS",
        )
        if test_support_arguments["name"][0].value != "test_support_lib":
            raise BoundaryError(
                f"{package} Bazel test-support rust_library must be named "
                "test_support_lib"
            )
        if [token.value for token in test_support_arguments.get("testonly", [])] != [
            "True"
        ]:
            raise BoundaryError(
                f"{package} Bazel test-support rust_library must be testonly"
            )
        if _literal_string_list(
            test_support_arguments.get("rustc_flags", []),
            package,
            "test-support rustc_flags",
        ) != ('--cfg=feature="test-support"',):
            raise BoundaryError(
                f"{package} Bazel test-support rust_library must enable only "
                "the test-support feature"
            )

    rust_tests = _find_calls(tokens, "ctx_rust_test", package)
    if not rust_tests:
        raise BoundaryError(f"{package} Bazel must define at least one ctx_rust_test")
    for index, rust_test in enumerate(rust_tests, start=1):
        _validate_rule_dependencies(
            rust_test,
            package,
            f"ctx_rust_test #{index}",
            {"normal": "True", "normal_dev": "True"},
            {"proc_macro": "True", "proc_macro_dev": "True"},
            "JSONL_TEST_DEPS" if jsonl else direct_dependencies,
        )

    dependency_inventory = (
        (
            ("JSONL_DEPS", JSONL_DIRECT_BAZEL_DEPENDENCIES, 2),
            (
                "JSONL_TEST_DEPS",
                JSONL_TEST_DIRECT_BAZEL_DEPENDENCIES,
                2 + len(rust_tests),
            ),
        )
        if jsonl
        else (
            (
                "RUNTIME_DEPS",
                RUNTIME_DIRECT_BAZEL_DEPENDENCIES,
                1 + len(rust_libraries) + len(rust_tests),
            ),
        )
    )
    for dependency_variable, expected_dependencies, expected_uses in dependency_inventory:
        assignments = _assignments(tokens, dependency_variable)
        if len(assignments) != 1 or assignments[0][0] != "=":
            raise BoundaryError(
                f"{package} Bazel must define exactly one unaugmented "
                f"{dependency_variable}"
            )
        if (
            _literal_string_list(assignments[0][1], package, dependency_variable)
            != expected_dependencies
        ):
            raise BoundaryError(
                f"{package} Bazel {dependency_variable} direct dependency inventory "
                "drifted"
            )
        actual_uses = sum(
            token.kind == "identifier" and token.value == dependency_variable
            for token in tokens
        )
        if actual_uses != expected_uses:
            raise BoundaryError(
                f"{package} Bazel {dependency_variable} may only be assigned once and "
                "used directly by dependency attributes"
            )


def _validate_runtime_bazel(build_path: Path) -> None:
    _validate_package_bazel(
        build_path, "ctx-history-capture-runtime", jsonl=False
    )


def _validate_jsonl_bazel(build_path: Path) -> None:
    _validate_package_bazel(build_path, "ctx-history-jsonl", jsonl=True)


def _validate_provider_bazel(build_path: Path) -> None:
    package = "ctx-history-provider-native-jsonl"
    tokens = _tokenize_starlark(build_path.read_text(encoding="utf-8"), package)
    _validate_canonical_loads(tokens, package, {"NATIVE_JSONL_DEPS", "NATIVE_JSONL_TEST_DEPS"})
    _validate_call_surface(tokens, package)

    rust_libraries = _find_calls(tokens, "rust_library", package)
    if len(rust_libraries) != 2:
        raise BoundaryError(
            f"{package} Bazel must define exactly 2 rust_library target(s)"
        )
    library_arguments = _validate_rule_dependencies(
        rust_libraries[0],
        package,
        "rust_library",
        {"normal": "True"},
        {"proc_macro": "True"},
        "NATIVE_JSONL_DEPS",
    )
    if library_arguments["name"][0].value != "lib":
        raise BoundaryError(f"{package} Bazel rust_library must be named lib")

    test_support_arguments = _validate_rule_dependencies(
        rust_libraries[1],
        package,
        "test-support rust_library",
        {"normal": "True", "normal_dev": "True"},
        {"proc_macro": "True", "proc_macro_dev": "True"},
        "NATIVE_JSONL_TEST_DEPS",
    )
    if test_support_arguments["name"][0].value != "test_support_lib":
        raise BoundaryError(
            f"{package} Bazel test-support rust_library must be named test_support_lib"
        )
    if [token.value for token in test_support_arguments.get("testonly", [])] != ["True"]:
        raise BoundaryError(
            f"{package} Bazel test-support rust_library must be testonly"
        )

    rust_tests = _find_calls(tokens, "ctx_rust_test", package)
    if not rust_tests:
        raise BoundaryError(f"{package} Bazel must define at least one ctx_rust_test")
    for index, rust_test in enumerate(rust_tests, start=1):
        _validate_rule_dependencies(
            rust_test,
            package,
            f"ctx_rust_test #{index}",
            {"normal": "True", "normal_dev": "True"},
            {"proc_macro": "True", "proc_macro_dev": "True"},
            "NATIVE_JSONL_TEST_DEPS",
        )

    dependency_inventory = (
        ("NATIVE_JSONL_DEPS", PROVIDER_DIRECT_BAZEL_DEPENDENCIES, 2),
        (
            "NATIVE_JSONL_TEST_DEPS",
            PROVIDER_TEST_DIRECT_BAZEL_DEPENDENCIES,
            2 + len(rust_tests),
        ),
    )
    for dependency_variable, expected_dependencies, expected_uses in dependency_inventory:
        assignments = _assignments(tokens, dependency_variable)
        if len(assignments) != 1 or assignments[0][0] != "=":
            raise BoundaryError(
                f"{package} Bazel must define exactly one unaugmented "
                f"{dependency_variable}"
            )
        if (
            _literal_string_list(assignments[0][1], package, dependency_variable)
            != expected_dependencies
        ):
            raise BoundaryError(
                f"{package} Bazel {dependency_variable} direct dependency inventory drifted"
            )
        actual_uses = sum(
            token.kind == "identifier" and token.value == dependency_variable
            for token in tokens
        )
        if actual_uses != expected_uses:
            raise BoundaryError(
                f"{package} Bazel {dependency_variable} may only be assigned once and "
                "used directly by dependency attributes"
            )


def _is_all_crate_deps(tokens: Sequence[Token], **expected: str) -> bool:
    if len(tokens) < 3 or tokens[0].value != "all_crate_deps" or tokens[1].value != "(":
        return False
    if tokens[-1].value != ")":
        return False
    arguments = _split_top_level(tokens[2:-1])
    actual: dict[str, str] = {}
    for argument in arguments:
        if (
            len(argument) != 3
            or argument[0].kind != "identifier"
            or argument[1].value != "="
            or argument[2].kind != "identifier"
        ):
            return False
        if argument[0].value in actual:
            return False
        actual[argument[0].value] = argument[2].value
    return actual == expected


def _validate_runtime_manifest(manifest_path: Path, workspace_manifest: dict[str, Any]) -> None:
    package = "ctx-history-capture-runtime"
    manifest = _read_manifest(manifest_path)
    _validate_no_forbidden_cargo_dependencies(
        manifest, package, RUNTIME_FORBIDDEN_CARGO, workspace_manifest
    )
    dependencies = manifest.get("dependencies", {})
    if dependencies != EXPECTED_RUNTIME_DEPENDENCIES:
        raise BoundaryError(
            "ctx-history-capture-runtime Cargo production dependencies drifted: "
            f"expected={sorted(EXPECTED_RUNTIME_DEPENDENCIES)} actual={sorted(dependencies)}"
        )
    if manifest.get("dev-dependencies", {}) != EXPECTED_RUNTIME_DEV_DEPENDENCIES:
        raise BoundaryError("ctx-history-capture-runtime Cargo dev dependencies drifted")
    build_dependencies = manifest.get("build-dependencies", {})
    if build_dependencies:
        raise BoundaryError(
            "ctx-history-capture-runtime Cargo build dependencies drifted: "
            f"expected=[] actual={sorted(build_dependencies)}"
        )
    target_dependency_tables = [
        table_name
        for table_name, _ in _dependency_tables(manifest, package)
        if table_name.startswith("target.")
    ]
    if target_dependency_tables:
        raise BoundaryError(
            "ctx-history-capture-runtime Cargo target-specific dependency-table "
            "bypass: "
            + ", ".join(target_dependency_tables)
        )


def _validate_provider_manifest(
    manifest_path: Path, workspace_manifest: dict[str, Any]
) -> None:
    package = "ctx-history-provider-native-jsonl"
    manifest = _read_manifest(manifest_path)
    _validate_no_forbidden_cargo_dependencies(
        manifest, package, PROVIDER_FORBIDDEN_CARGO, workspace_manifest
    )
    dependencies = manifest.get("dependencies", {})
    if dependencies != EXPECTED_PROVIDER_DEPENDENCIES:
        raise BoundaryError(
            "ctx-history-provider-native-jsonl Cargo production dependencies drifted: "
            f"expected={sorted(EXPECTED_PROVIDER_DEPENDENCIES)} actual={sorted(dependencies)}"
        )
    if manifest.get("dev-dependencies", {}) != EXPECTED_PROVIDER_DEV_DEPENDENCIES:
        raise BoundaryError(
            "ctx-history-provider-native-jsonl Cargo dev dependencies drifted"
        )
    build_dependencies = manifest.get("build-dependencies", {})
    if build_dependencies:
        raise BoundaryError(
            "ctx-history-provider-native-jsonl Cargo build dependencies drifted: "
            f"expected=[] actual={sorted(build_dependencies)}"
        )
    target_dependency_tables = [
        table_name
        for table_name, _ in _dependency_tables(manifest, package)
        if table_name.startswith("target.")
    ]
    if target_dependency_tables:
        raise BoundaryError(
            "ctx-history-provider-native-jsonl Cargo target-specific dependency-table "
            "bypass: "
            + ", ".join(target_dependency_tables)
        )


def validate(
    workspace_manifest_path: Path,
    runtime_manifest_path: Path,
    runtime_build_path: Path,
    jsonl_manifest_path: Path,
    jsonl_build_path: Path,
    provider_manifest_path: Path,
    provider_build_path: Path,
) -> None:
    workspace_manifest = _read_manifest(workspace_manifest_path)
    _validate_runtime_manifest(runtime_manifest_path, workspace_manifest)
    _validate_runtime_bazel(runtime_build_path)
    jsonl_manifest = _read_manifest(jsonl_manifest_path)
    _validate_no_forbidden_cargo_dependencies(
        jsonl_manifest,
        "ctx-history-jsonl",
        JSONL_FORBIDDEN_CARGO,
        workspace_manifest,
    )
    _validate_jsonl_bazel(jsonl_build_path)
    _validate_provider_manifest(provider_manifest_path, workspace_manifest)
    _validate_provider_bazel(provider_build_path)


def main() -> int:
    if len(sys.argv) != 8:
        raise SystemExit(
            "usage: check_history_capture_runtime_boundary.py "
            "WORKSPACE_CARGO RUNTIME_CARGO RUNTIME_BUILD JSONL_CARGO JSONL_BUILD "
            "PROVIDER_CARGO PROVIDER_BUILD"
        )
    try:
        validate(
            Path(sys.argv[1]),
            Path(sys.argv[2]),
            Path(sys.argv[3]),
            Path(sys.argv[4]),
            Path(sys.argv[5]),
            Path(sys.argv[6]),
            Path(sys.argv[7]),
        )
    except (BoundaryError, OSError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    print("history runtime/JSONL/provider static dependency boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
