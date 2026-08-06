#!/usr/bin/env python3
"""Validate the complete checksum contract for native Bazel Rust toolchains."""

from __future__ import annotations

import ast
import json
from pathlib import Path
import sys


VERSION = "1.97.1"
RULES_RUST_VERSION = "0.71.3"
RULES_RUST_PATCHES = [
    "//tools/bazel/patches:rules-rust-freebsd-host.patch",
    "//tools/bazel/patches:rules-rust-windows-cargo-runfiles.patch",
    "//tools/bazel/patches:rules-rust-windows-gnu-dlltool-path.patch",
]
WINDOWS_CARGO_RUNFILES_PATCH_LINES = (
    "diff --git a/cargo/private/cargo_build_script_runner/cargo_manifest_dir.rs ",
    "-            if !self",
    "+            if self",
)
WINDOWS_GNU_DLLTOOL_PATCH_LINES = (
    'diff --git a/rust/private/rustc.bzl b/rust/private/rustc.bzl',
    '+load("@bazel_skylib//lib:paths.bzl", "paths")',
    '+    if toolchain.target_os == "windows" and toolchain.target_abi == "gnu" and cc_toolchain:',
    '+            action_name = CPP_LINK_EXECUTABLE_ACTION_NAME,',
    '+        tool_dir = paths.dirname(linker)',
    '+        env["PATH"] = tool_dir + (";" + inherited_path if inherited_path else "")',
)
CHANNEL_MANIFEST = (
    "https://static.rust-lang.org/dist/2026-07-16/channel-rust-1.97.1.toml"
)
PLATFORM_HOST_TRIPLES = {
    ("freebsd", "x86_64"): "x86_64-unknown-freebsd",
    ("linux", "aarch64"): "aarch64-unknown-linux-gnu",
    ("linux", "x86_64"): "x86_64-unknown-linux-gnu",
    ("macos", "aarch64"): "aarch64-apple-darwin",
    ("macos", "x86_64"): "x86_64-apple-darwin",
    # rules_rust's native Windows host toolchain is MSVC. The public GNU
    # artifact is constructed by Cargo and is not a second native Bazel host.
    ("windows", "x86_64"): "x86_64-pc-windows-msvc",
}
HOST_SHA256S = {
    "aarch64-apple-darwin": {
        "cargo": "2d84a74e9558192a7de674aca6aa3ab7464bed2df97e0377156ddb7e09a0fd7a",
        "clippy": "5e44c0ac5ca9b6f14a3c9031a61f583348b902f908f46e95717aef1dbd2807db",
        "llvm-tools": "2160fccee889a092a281bd6d90c09519ca75ab9ed34d9d86301f4fb9602382f8",
        "rust-std": "a4895f5c6995e83cab8687e46b14324592398049def71ce75ca308c981cf200d",
        "rustc": "6076cad38ccabaa24325f26a74080a363a2633a9cd34c473a8977255d8a593cb",
        "rustfmt": "358bbba5d0c7c37116ec15f67cfd3ac4da5d3c319cddb49389c26d3a0c65747a",
    },
    "aarch64-unknown-linux-gnu": {
        "cargo": "8f70bcaccea5ba4db187c3fd4d64e24592b4e16af513497201f5909d61691dbe",
        "clippy": "d8bac7b0ba5ca9bb868ccb9e367a1d52f4837f3ebf4892eaf64cda37ce362bb5",
        "llvm-tools": "1c471f218346d37bdccadb1a9d37383d448cdaf00bf2a7cb1072c0970eb48b40",
        "rust-std": "46aed8e63186350004d8ec6afca798811e6530b514352e5a8a26f3dc4939b3be",
        "rustc": "b344b81f0cd4c2246c7da8b197fe7a339d7dd02bb15cb69b2524115d9c75224c",
        "rustfmt": "3dbde15d30794924195ae446f3d2ceb542a131306d22ae7912c7634d414622a8",
    },
    "x86_64-apple-darwin": {
        "cargo": "1bd1029b579d0563ca851ebd095914871535bfd1978a123eeaa03107e89b0e03",
        "clippy": "6dad187a2210db93c63cdf21a376db8b7fe4f5e64d6ef4d404a74d166e59ad74",
        "llvm-tools": "b8c7fd75bf79873177b51c008fdac5ef785950aa6d2c11d5250053176a16549a",
        "rust-std": "0fa78653023be5bdfeb419edc82e3b1346ccaa23eaa036491cce084101c741dd",
        "rustc": "3c38289f319bf02fa1c8149ce3e00f261e4efd14813a99f7f7ae4f180c7d1173",
        "rustfmt": "457c35a619207d35da2a3804940e620ad7cdc8e0808b17f2f6c2202f9e3f3d91",
    },
    "x86_64-pc-windows-msvc": {
        "cargo": "1180ac0cd30ee98af682528c10505f5cba118f122aec9b7ca18ae605b1db38a0",
        "clippy": "3be927ecfbbba535bee2f4d23cd08c639278c8746c04bd70fb21ea58667b054b",
        "llvm-tools": "4f51224d60b67ed343079cbc1743143e97e7ec8a540ad3d056ac0a2a8eb532e8",
        "rust-std": "05f356609926e663a81e9697077214236514b2f9ff7a36e63b0070f43f073f66",
        "rustc": "0119e2788f3391a891b2e0fe611e82b433670eeae76c45995b081d0ac7715c6d",
        "rustfmt": "1718eea97fc34543c71a5849bc72952f88bc6c789c9110626bc00b2791642e81",
    },
    "x86_64-unknown-freebsd": {
        "cargo": "aa79c6488e9443fb29e23cf5200d221408e9a7059d95127c5ce7f5123b8995b5",
        "clippy": "c46ab554746a99f99ebd83bfca4546e35b13e0728f5ea0ad61022d3c4ba753d1",
        "llvm-tools": "67adad3ba4f9660714a19c24da99a883fa7f924bfca0d754571ebabf430bf63c",
        "rust-std": "9decb923c4f2b1fc57d2dcb39a8c9c96f91b1de586abf37cfe2db9edf4530d43",
        "rustc": "bd0e697c35369eb2836b864cda764d3d9c878f2f3f3a99137983fcf581a82959",
        "rustfmt": "1982a27f1d0d5bd6a4248d06cb13f63cdd95f9782742ad3a180b6314a4cfb889",
    },
    "x86_64-unknown-linux-gnu": {
        "cargo": "e1be5f5ff7f7f80ca506fb65770b759edbdc6d303781ed71c5de8ec8a8394779",
        "clippy": "3441df8fb54db985f8c8a3e8356b8874a3f92cc8cca8565cfe36f1dc15935e72",
        "llvm-tools": "3e7a1c596a42dea6bf625ec6f006ce2fbcf5d1ff892f082828eb182a5d483b95",
        "rust-std": "1c1e704ae80126b7de34f72ea2825f7fd01736dec20732faed47374b95282fba",
        "rustc": "9819d0a32d56bd339585319c80260e332779f5541fd66838ab7e016d6c814819",
        "rustfmt": "907fe97d6afbde1eca1b34c992c76e1406d422e2e6f137813d382acec7eb4d14",
    },
}


class PinContractError(ValueError):
    """The MODULE.bazel Rust toolchain declaration is incomplete or incorrect."""


def expected_sha256s() -> dict[str, str]:
    return {
        f"{component}-{VERSION}-{triple}.tar.xz": checksum
        for triple, components in HOST_SHA256S.items()
        for component, checksum in components.items()
    }


def _literal(node: ast.AST, assignments: dict[str, ast.AST]) -> object:
    if isinstance(node, ast.Name):
        try:
            node = assignments[node.id]
        except KeyError as error:
            raise PinContractError(f"unknown MODULE.bazel value {node.id}") from error
    try:
        return ast.literal_eval(node)
    except (ValueError, TypeError) as error:
        raise PinContractError("Rust toolchain contract values must be literals") from error


def validate_module_text(module_text: str) -> None:
    tree = ast.parse(module_text, filename="MODULE.bazel")
    assignments = {
        node.targets[0].id: node.value
        for node in tree.body
        if isinstance(node, ast.Assign)
        and len(node.targets) == 1
        and isinstance(node.targets[0], ast.Name)
    }
    override_calls = [
        node
        for node in tree.body
        if isinstance(node, ast.Expr)
        and isinstance(node.value, ast.Call)
        and isinstance(node.value.func, ast.Name)
        and node.value.func.id == "single_version_override"
    ]
    rules_rust_overrides = []
    for node in override_calls:
        keywords = {
            keyword.arg: keyword.value
            for keyword in node.value.keywords
            if keyword.arg
        }
        if "module_name" in keywords and _literal(
            keywords["module_name"], assignments
        ) == "rules_rust":
            rules_rust_overrides.append(keywords)
    if len(rules_rust_overrides) != 1:
        raise PinContractError("expected exactly one rules_rust version override")
    rules_rust_override = rules_rust_overrides[0]
    if (
        _literal(rules_rust_override.get("version"), assignments)
        != RULES_RUST_VERSION
        or _literal(rules_rust_override.get("patch_strip"), assignments) != 1
        or _literal(rules_rust_override.get("patches"), assignments)
        != RULES_RUST_PATCHES
    ):
        raise PinContractError("rules_rust override has incomplete release patches")
    calls = [
        node
        for node in ast.walk(tree)
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and isinstance(node.func.value, ast.Name)
        and node.func.value.id == "rust"
        and node.func.attr == "toolchain"
    ]
    if len(calls) != 1:
        raise PinContractError("expected exactly one rust.toolchain declaration")
    keywords = {keyword.arg: keyword.value for keyword in calls[0].keywords if keyword.arg}

    def keyword(name: str) -> object:
        if name not in keywords:
            raise PinContractError(f"rust.toolchain is missing {name}")
        return _literal(keywords[name], assignments)

    if keyword("versions") != [VERSION] or keyword("rustfmt_version") != VERSION:
        raise PinContractError(f"Rust and rustfmt must both use {VERSION}")
    actual = keyword("sha256s")
    if not isinstance(actual, dict):
        raise PinContractError("sha256s must be a dictionary")
    expected = expected_sha256s()
    missing = sorted(set(expected) - set(actual))
    if missing:
        raise PinContractError(f"missing checksum for {missing[0]}")
    for archive, checksum in expected.items():
        if actual[archive] != checksum:
            raise PinContractError(
                f"wrong checksum for {archive}: expected {checksum}, got {actual[archive]}"
            )

    crate_calls = [
        node
        for node in ast.walk(tree)
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and isinstance(node.func.value, ast.Name)
        and node.func.value.id == "crate"
        and node.func.attr == "from_cargo"
    ]
    if len(crate_calls) != 1:
        raise PinContractError("expected exactly one crate.from_cargo declaration")
    crate_keywords = {
        keyword.arg: keyword.value
        for keyword in crate_calls[0].keywords
        if keyword.arg
    }
    triples_node = crate_keywords.get("supported_platform_triples")
    if triples_node is None:
        raise PinContractError(
            "crate.from_cargo is missing supported_platform_triples"
        )
    triples = _literal(triples_node, assignments)
    if not isinstance(triples, list) or any(
        not isinstance(triple, str) for triple in triples
    ):
        raise PinContractError("supported_platform_triples must be a string list")
    missing_triples = sorted(set(HOST_SHA256S) - set(triples))
    if missing_triples:
        raise PinContractError(
            "crate_universe is missing release host triple "
            f"{missing_triples[0]}"
        )


def validate_windows_gnu_dlltool_patch_text(patch_text: str) -> None:
    for line in WINDOWS_GNU_DLLTOOL_PATCH_LINES:
        if line not in patch_text:
            raise PinContractError(
                "Windows-GNU dlltool discovery patch is missing: " + line
            )


def validate_windows_cargo_runfiles_patch_text(patch_text: str) -> None:
    for line in WINDOWS_CARGO_RUNFILES_PATCH_LINES:
        if line not in patch_text:
            raise PinContractError(
                "Windows Cargo runfiles retention patch is missing: " + line
            )


def validate_release_matrix_text(matrix_text: str) -> None:
    try:
        matrix = json.loads(matrix_text)
        targets = matrix["targets"]
    except (json.JSONDecodeError, KeyError, TypeError) as error:
        raise PinContractError("release target matrix is malformed") from error

    actual: set[str] = set()
    for target in targets:
        try:
            platform = (target["os"], target["arch"])
            actual.add(PLATFORM_HOST_TRIPLES[platform])
        except (KeyError, TypeError) as error:
            raise PinContractError(
                f"release target has no native Bazel host mapping: {target!r}"
            ) from error
    expected = set(HOST_SHA256S)
    if actual != expected:
        missing = sorted(expected - actual)
        unexpected = sorted(actual - expected)
        raise PinContractError(
            f"release host matrix mismatch: missing={missing}, unexpected={unexpected}"
        )


def main() -> int:
    if len(sys.argv) != 5:
        print(
            "usage: check_rust_toolchain_pins.py MODULE.bazel "
            "release-targets.json rules-rust-windows-gnu-dlltool-path.patch "
            "rules-rust-windows-cargo-runfiles.patch",
            file=sys.stderr,
        )
        return 2
    try:
        validate_module_text(Path(sys.argv[1]).read_text(encoding="utf-8"))
        validate_release_matrix_text(Path(sys.argv[2]).read_text(encoding="utf-8"))
        validate_windows_gnu_dlltool_patch_text(
            Path(sys.argv[3]).read_text(encoding="utf-8")
        )
        validate_windows_cargo_runfiles_patch_text(
            Path(sys.argv[4]).read_text(encoding="utf-8")
        )
    except (OSError, SyntaxError, PinContractError) as error:
        print(f"Rust toolchain pin contract failed: {error}", file=sys.stderr)
        return 1
    print(
        f"Rust toolchain pins: OK ({len(HOST_SHA256S)} hosts, "
        f"{len(expected_sha256s())} archives; {CHANNEL_MANIFEST})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
