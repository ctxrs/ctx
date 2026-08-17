#!/usr/bin/env python3
"""Validate the complete public CI entrypoint and nested suite contract."""

from __future__ import annotations

import ast
from pathlib import Path
import sys


CI_LINT_CONFIG = "build:ci --config=lint"
CLIPPY_ASPECT = (
    "build:lint --aspects=@rules_rust//rust:defs.bzl%rust_clippy_aspect"
)
CLIPPY_OUTPUT = "build:lint --output_groups=+clippy_checks"
CLIPPY_WARNINGS = (
    "build:lint --@rules_rust//rust/settings:clippy_flag=-Dwarnings"
)


class ContractError(ValueError):
    """The public CI entrypoint is incomplete or internally inconsistent."""


def _active_lines(text: str) -> list[str]:
    return [
        line.strip()
        for line in text.splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]


def validate_bazelrc_text(bazelrc_text: str) -> None:
    lines = _active_lines(bazelrc_text)
    for required in (CI_LINT_CONFIG, CLIPPY_ASPECT, CLIPPY_OUTPUT, CLIPPY_WARNINGS):
        if lines.count(required) != 1:
            raise ContractError(f"expected exactly one {required!r}")

    ci_lines = [line for line in lines if line.startswith("build:ci ")]
    if any(
        "rust_clippy_aspect" in line
        or "clippy_checks" in line
        or "clippy_flag" in line
        for line in ci_lines
    ):
        raise ContractError("build:ci must inherit, not duplicate, the lint settings")

    clippy_flags = [line for line in lines if "clippy_flag=" in line]
    if clippy_flags != [CLIPPY_WARNINGS]:
        raise ContractError("the lint config must have exactly one -Dwarnings flag")


def validate_check_text(check_text: str) -> None:
    marker = 'case "${mode}" in'
    start = check_text.rfind(marker)
    if start < 0:
        raise ContractError("scripts/check.sh is missing its mode execution case")
    execution = _active_lines(check_text[start:])
    expected = [
        marker,
        "ci)",
        "run_bazel build //... --config=ci",
        "run_bazel test //:ci_tests --config=test",
        ";;",
        "nightly)",
        "run_bazel build //... --config=ci",
        "run_bazel test //:nightly_tests --config=test",
        ";;",
        "release)",
        "run_bazel build //... --config=ci",
        "run_bazel test //:nightly_tests --config=test",
        ";;",
        "esac",
    ]
    if execution != expected:
        raise ContractError(
            "named modes must lint-build //... with --config=ci, then run only "
            "their deterministic *_tests suite with --config=test"
        )


def _call_name(call: ast.Call) -> str | None:
    for keyword in call.keywords:
        if keyword.arg == "name":
            try:
                value = ast.literal_eval(keyword.value)
            except (TypeError, ValueError):
                return None
            return value if isinstance(value, str) else None
    return None


def _keyword(call: ast.Call, name: str) -> ast.AST | None:
    return next(
        (keyword.value for keyword in call.keywords if keyword.arg == name),
        None,
    )


def _shape(expression: str) -> str:
    return ast.dump(ast.parse(expression, mode="eval").body, include_attributes=False)


def validate_build_text(build_text: str) -> None:
    try:
        tree = ast.parse(build_text, filename="BUILD.bazel")
    except SyntaxError as error:
        raise ContractError(f"BUILD.bazel cannot be parsed: {error}") from error

    retired = {"ci", "nightly", "release"}
    for node in ast.walk(tree):
        if isinstance(node, ast.Call) and _call_name(node) in retired:
            raise ContractError("retired ambiguous root suite name remains")

    suites: dict[str, ast.Call] = {}
    for node in tree.body:
        if not (
            isinstance(node, ast.Expr)
            and isinstance(node.value, ast.Call)
            and isinstance(node.value.func, ast.Name)
            and node.value.func.id == "test_suite"
        ):
            continue
        name = _call_name(node.value)
        if name in {"ci_tests", "nightly_tests"}:
            if name in suites:
                raise ContractError(f"duplicate //:{name} suite")
            suites[name] = node.value

    expected = {
        "ci_tests": _shape("CI_TESTS"),
        "nightly_tests": _shape('[":ci_tests"] + NIGHTLY_TESTS'),
    }
    if set(suites) != set(expected):
        raise ContractError("the ci_tests/nightly_tests suites are required")
    for name, expected_tests in expected.items():
        tests = _keyword(suites[name], "tests")
        if tests is None or ast.dump(tests, include_attributes=False) != expected_tests:
            raise ContractError(f"//:{name} has incorrect nesting or inventory")


def main() -> int:
    if len(sys.argv) != 4:
        print(
            "usage: check_ci_entrypoint_contract.py .bazelrc BUILD.bazel "
            "scripts/check.sh",
            file=sys.stderr,
        )
        return 2
    try:
        validate_bazelrc_text(Path(sys.argv[1]).read_text(encoding="utf-8"))
        validate_build_text(Path(sys.argv[2]).read_text(encoding="utf-8"))
        validate_check_text(Path(sys.argv[3]).read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, ContractError) as error:
        print(f"public CI entrypoint contract failed: {error}", file=sys.stderr)
        return 1
    print("public CI entrypoint contract: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
