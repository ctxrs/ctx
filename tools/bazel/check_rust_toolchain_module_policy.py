#!/usr/bin/env python3
"""Keep the small rules_rust platform-policy declarations explicit."""

import ast
from pathlib import Path
import sys


PATCHES = (
    "//tools/bazel/patches:rules-rust-freebsd-host.patch",
    "//tools/bazel/patches:rules-rust-windows-cargo-runfiles.patch",
    "//tools/bazel/patches:rules-rust-windows-gnu-dlltool-path.patch",
)
FREEBSD = "x86_64-unknown-freebsd"


class PolicyError(ValueError):
    pass


def _fields(call: ast.Call) -> dict[str, object]:
    try:
        return {item.arg: ast.literal_eval(item.value) for item in call.keywords if item.arg}
    except (TypeError, ValueError) as error:
        raise PolicyError("toolchain policy declarations must use literals") from error


def validate(text: str) -> None:
    tree = ast.parse(text, filename="MODULE.bazel")
    overrides = [
        _fields(node.value) for node in tree.body
        if isinstance(node, ast.Expr) and isinstance(node.value, ast.Call)
        and isinstance(node.value.func, ast.Name)
        and node.value.func.id == "single_version_override"
        and _fields(node.value).get("module_name") == "rules_rust"
    ]
    crates = [
        _fields(node.value) for node in tree.body
        if isinstance(node, ast.Expr) and isinstance(node.value, ast.Call)
        and isinstance(node.value.func, ast.Attribute)
        and isinstance(node.value.func.value, ast.Name)
        and node.value.func.value.id == "crate" and node.value.func.attr == "from_cargo"
        and _fields(node.value).get("name") == "crates"
    ]
    if len(overrides) != 1 or overrides[0].get("patch_strip") != 1 or not set(PATCHES) <= set(overrides[0].get("patches", ())):
        raise PolicyError("rules_rust override lacks required patches or patch_strip")
    triples = crates[0].get("supported_platform_triples", ()) if len(crates) == 1 else ()
    if not isinstance(triples, (list, tuple)) or FREEBSD not in triples:
        raise PolicyError("crate_universe lacks required FreeBSD platform")


def _rejected(text: str) -> None:
    try:
        validate(text)
    except (PolicyError, SyntaxError):
        return
    raise PolicyError("toolchain policy mutation passed")


def main() -> int:
    text = Path(sys.argv[1]).read_text(encoding="utf-8") if len(sys.argv) == 2 else Path("MODULE.bazel").read_text(encoding="utf-8")
    try:
        validate(text)
        for value in PATCHES:
            _rejected(text.replace(value, "", 1))
            _rejected(text.replace(value, value + ".changed", 1))
        _rejected(text.replace("patch_strip = 1,", "", 1))
        _rejected(text.replace("patch_strip = 1,", "patch_strip = 2,", 1))
        marker = f'"{FREEBSD}"'
        _rejected(text.replace(marker, "", 1))
        _rejected(text.replace(marker, '"changed-freebsd"', 1))
    except (OSError, SyntaxError, PolicyError) as error:
        print(f"Rust toolchain module policy failed: {error}", file=sys.stderr)
        return 1
    print("Rust toolchain module policy: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
