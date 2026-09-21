#!/usr/bin/env python3
"""Keep Rust compiler pins aligned and platform-policy declarations explicit."""

import ast
from pathlib import Path
import re
import sys

try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib


PATCHES = (
    "//tools/bazel/patches:rules-rust-freebsd-host.patch",
    "//tools/bazel/patches:rules-rust-windows-cargo-runfiles.patch",
    "//tools/bazel/patches:rules-rust-windows-gnu-dlltool-path.patch",
)
FREEBSD = "x86_64-unknown-freebsd"
PIN_PATTERNS = {
    ".buildkite/pipeline.yml": r'^  CTX_RUST_TOOLCHAIN: "([^"]+)"$',
    "scripts/buildkite-public-ci.sh": r'^export CTX_RUST_TOOLCHAIN="\$\{CTX_RUST_TOOLCHAIN:-([^}]+)\}"$',
    "scripts/real-harness-common.sh": r'^  export RUSTUP_TOOLCHAIN="\$\{RUSTUP_TOOLCHAIN:-\$\{CTX_RUST_TOOLCHAIN:-([^}]+)\}\}"$',
    "scripts/release/build-public-candidate-on-linux.sh": r'^readonly RUST_VERSION="([^"]+)"$',
}


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


def validate_pins(files: dict[str, str]) -> None:
    pin = tomllib.loads(files["rust-toolchain.toml"])["toolchain"]
    version = pin.get("channel", "")
    if not isinstance(version, str) or not re.fullmatch(r"1\.\d+\.\d+", version):
        raise PolicyError("rust-toolchain.toml must pin an exact stable release")
    if pin.get("profile") != "minimal" or set(pin.get("components", ())) != {"rustfmt", "clippy"}:
        raise PolicyError("repository toolchain requires minimal, rustfmt, and clippy")
    manifest = tomllib.loads(files["Cargo.toml"])
    if manifest["workspace"]["package"]["rust-version"] != "1.95":
        raise PolicyError("the retained repository parser requires the Rust 1.95 MSRV")
    primary_count = 0
    for node in ast.walk(ast.parse(files["MODULE.bazel"])):
        if not (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
                and isinstance(node.func.value, ast.Name) and node.func.value.id == "rust"
                and node.func.attr in {"toolchain", "repository_set"}):
            continue
        fields = _fields(node)
        primary_count += node.func.attr == "toolchain"
        # A repository_set continuation only adds another target to its named set.
        if node.func.attr == "repository_set" and not {"versions", "rustfmt_version", "sha256s"} & fields.keys():
            continue
        if fields.get("versions") != [version] or fields.get("rustfmt_version") != version:
            raise PolicyError("Bazel compiler and rustfmt must match rust-toolchain.toml")
        archives = fields.get("sha256s", {})
        archive_name = rf"(?:cargo|clippy|llvm-tools|rust-std|rustc|rustfmt)-{re.escape(version)}-.+\.tar\.xz"
        if not archives or any(not re.fullmatch(archive_name, name)
                               or not re.fullmatch(r"[0-9a-f]{64}", digest)
                               for name, digest in archives.items()):
            raise PolicyError("Bazel archives must have matching versions and SHA-256 pins")
    if primary_count != 1:
        raise PolicyError("expected one primary Rust toolchain")
    for path, pattern in PIN_PATTERNS.items():
        if re.findall(pattern, files[path], re.MULTILINE) != [version]:
            raise PolicyError(f"{path} must match rust-toolchain.toml")


def check_pin_mutations(files: dict[str, str]) -> None:
    version = tomllib.loads(files["rust-toolchain.toml"])["toolchain"]["channel"]
    mutations = [(path, version, "0.0.0") for path in files
                 if path not in {"Cargo.toml", "MODULE.bazel"}]
    mutations += [
        ("rust-toolchain.toml", version, "stable"),
        ("rust-toolchain.toml", 'profile = "minimal"', 'profile = "default"'),
        ("rust-toolchain.toml", '"clippy"', '"rust-src"'),
        ("Cargo.toml", 'rust-version = "1.95"', 'rust-version = "1.98"'),
        ("MODULE.bazel", f'versions = ["{version}"]', 'versions = ["0.0.0"]'),
        ("MODULE.bazel", f'rustfmt_version = "{version}"', 'rustfmt_version = "0.0.0"'),
        ("MODULE.bazel", f'cargo-{version}-', 'cargo-0.0.0-'),
    ]
    for path, old, new in mutations:
        changed = files[path].replace(old, new, 1)
        if changed == files[path]:
            raise PolicyError(f"pin mutation did not change {path}")
        try:
            validate_pins({**files, path: changed})
        except (PolicyError, ValueError, KeyError):
            continue
        raise PolicyError(f"compiler pin mutation passed: {path}: {old}")


def main() -> int:
    module = Path(sys.argv[1]) if len(sys.argv) == 2 else Path("MODULE.bazel")
    try:
        files = {path: (module.parent / path).read_text(encoding="utf-8")
                 for path in ("MODULE.bazel", "rust-toolchain.toml", "Cargo.toml", *PIN_PATTERNS)}
        text = files["MODULE.bazel"]
        validate(text)
        validate_pins(files)
        check_pin_mutations(files)
        for value in PATCHES:
            _rejected(text.replace(value, "", 1))
            _rejected(text.replace(value, value + ".changed", 1))
        _rejected(text.replace("patch_strip = 1,", "", 1))
        _rejected(text.replace("patch_strip = 1,", "patch_strip = 2,", 1))
        marker = f'"{FREEBSD}"'
        _rejected(text.replace(marker, "", 1))
        _rejected(text.replace(marker, '"changed-freebsd"', 1))
    except (OSError, SyntaxError, ValueError, KeyError) as error:
        print(f"Rust toolchain module policy failed: {error}", file=sys.stderr)
        return 1
    print("Rust toolchain module policy: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
