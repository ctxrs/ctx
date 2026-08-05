#!/usr/bin/env python3
"""Validate the pinned FreeBSD ONNX Runtime shutdown-order repair."""

from __future__ import annotations

import ast
import hashlib
from pathlib import Path
import sys
import tomllib


ORT_COMMIT = "f7994dc91b8f48c78afc506880b3f9f558957919"
ORT_PATCH = "//tools/bazel/patches:ort-freebsd-release-env-order.patch"
ORT_PATCH_SHA256 = "effab3c783745774ee1079e1fe37f6850042be2477cf26f17f76de6f26b3f73e"
ORT_VERSION = "2.0.0-rc.12"


class ContractError(ValueError):
    """The FreeBSD ORT release contract is absent or internally inconsistent."""


def _literal(node: ast.AST) -> object:
    try:
        return ast.literal_eval(node)
    except (TypeError, ValueError) as error:
        raise ContractError("ORT annotation values must be literals") from error


def validate_module_text(module_text: str) -> None:
    tree = ast.parse(module_text, filename="MODULE.bazel")
    annotations: list[dict[str, object]] = []
    for node in ast.walk(tree):
        if (
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and isinstance(node.func.value, ast.Name)
            and node.func.value.id == "crate"
            and node.func.attr == "annotation"
        ):
            values = {
                keyword.arg: _literal(keyword.value)
                for keyword in node.keywords
                if keyword.arg
            }
            if values.get("crate") == "ort":
                annotations.append(values)
    if len(annotations) != 1:
        raise ContractError("expected exactly one crate.annotation for ort")
    annotation = annotations[0]
    expected = {
        "crate": "ort",
        "patch_args": ["-p1"],
        "patches": [ORT_PATCH],
        "version": ORT_VERSION,
    }
    if annotation != expected:
        raise ContractError(
            f"ORT annotation must be exactly {expected!r}, got {annotation!r}"
        )


def validate_cargo_text(cargo_text: str) -> None:
    try:
        dependency = tomllib.loads(cargo_text)["patch"]["crates-io"]["ort"]
    except (tomllib.TOMLDecodeError, KeyError, TypeError) as error:
        raise ContractError("Cargo.toml is missing the patched ort dependency") from error
    expected = {
        "git": "https://github.com/ctxrs/ort.git",
        "rev": ORT_COMMIT,
    }
    if dependency != expected:
        raise ContractError(
            f"ORT Cargo pin must be exactly {expected!r}, got {dependency!r}"
        )


def validate_patch_bytes(patch_bytes: bytes) -> None:
    digest = hashlib.sha256(patch_bytes).hexdigest()
    if digest != ORT_PATCH_SHA256:
        raise ContractError(
            f"ORT FreeBSD patch digest mismatch: expected {ORT_PATCH_SHA256}, got {digest}"
        )
    patch_text = patch_bytes.decode("utf-8")
    required = (
        '+#[cfg(all(not(windows), not(target_vendor = "apple"), not(target_os = "freebsd"), not(target_arch = "wasm32")))]',
        '+#[cfg(any(target_vendor = "apple", target_os = "freebsd"))]',
        "+\t#[cfg(any(target_vendor = \"apple\", target_os = \"freebsd\"))]",
        "+unsafe extern \"C\" fn release_env_on_exit(#[cfg(any(target_vendor = \"apple\", target_os = \"freebsd\"))] _: *const ())",
    )
    for line in required:
        if line not in patch_text:
            raise ContractError(f"ORT FreeBSD patch is missing {line!r}")


def main() -> int:
    if len(sys.argv) != 4:
        print(
            "usage: check_freebsd_ort_exit_contract.py "
            "MODULE.bazel Cargo.toml ort-freebsd-release-env-order.patch",
            file=sys.stderr,
        )
        return 2
    try:
        validate_module_text(Path(sys.argv[1]).read_text(encoding="utf-8"))
        validate_cargo_text(Path(sys.argv[2]).read_text(encoding="utf-8"))
        validate_patch_bytes(Path(sys.argv[3]).read_bytes())
    except (OSError, SyntaxError, UnicodeDecodeError, ContractError) as error:
        print(f"FreeBSD ORT exit contract failed: {error}", file=sys.stderr)
        return 1
    print(
        "FreeBSD ORT exit contract: OK "
        f"({ORT_COMMIT}, {ORT_PATCH_SHA256})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
