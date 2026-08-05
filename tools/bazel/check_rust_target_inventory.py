#!/usr/bin/env python3
"""Validate complete Cargo target metadata and exact native Bazel ownership."""

from __future__ import annotations

import argparse
import ast
import importlib.util
import json
from pathlib import Path
import re
import sys
from typing import Any


RUST_RULES = {
    "rust_binary",
    "rust_library",
    "rust_proc_macro",
    "rust_test",
    "ctx_rust_binary",
    "ctx_rust_test",
    "ctx_cli_integration_test",
}


def fail(message: str) -> None:
    raise SystemExit(f"rust target inventory check failed: {message}")


def load_gate(root: Path) -> Any:
    path = root / "scripts/check-crate-loc.py"
    if not path.is_file():
        fail(f"missing shared Cargo metadata implementation: {path}")
    sys.path.insert(0, str(path.parent))
    spec = importlib.util.spec_from_file_location("ctx_crate_gate", path)
    if spec is None or spec.loader is None:
        fail("could not load shared Cargo metadata implementation")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def _call_end(text: str, opening: int) -> int:
    depth = 1
    index = opening + 1
    quote: str | None = None
    triple = False
    while index < len(text):
        if quote is not None:
            marker = quote * (3 if triple else 1)
            if text.startswith(marker, index):
                quote = None
                index += len(marker)
            elif text[index] == "\\":
                index += 2
            else:
                index += 1
            continue
        if text[index] == "#":
            newline = text.find("\n", index)
            index = len(text) if newline < 0 else newline + 1
            continue
        if text[index] in {'"', "'"}:
            quote = text[index]
            triple = text.startswith(quote * 3, index)
            index += 3 if triple else 1
            continue
        if text[index] == "(":
            depth += 1
        elif text[index] == ")":
            depth -= 1
            if depth == 0:
                return index
        index += 1
    fail("unbalanced call in Bazel source")
    raise AssertionError


def calls(text: str) -> list[tuple[str, str]]:
    result: list[tuple[str, str]] = []
    pattern = r"(?m)^(?:[A-Za-z_][A-Za-z0-9_]*\s*=\s*)?([A-Za-z_][A-Za-z0-9_]*)\("
    for match in re.finditer(pattern, text):
        end = _call_end(text, match.end() - 1)
        result.append((match.group(1), text[match.end() : end]))
    return result


def call_attributes(body: str) -> dict[str, str]:
    fields: list[str] = []
    start = 0
    depths = {"(": 0, "[": 0, "{": 0}
    closing = {")": "(", "]": "[", "}": "{"}
    quote: str | None = None
    index = 0
    while index < len(body):
        character = body[index]
        if quote is not None:
            if character == "\\":
                index += 2
                continue
            if character == quote:
                quote = None
        elif character in {'"', "'"}:
            quote = character
        elif character == "#":
            newline = body.find("\n", index)
            index = len(body) if newline < 0 else newline
        elif character in depths:
            depths[character] += 1
        elif character in closing:
            depths[closing[character]] -= 1
        elif character == "," and not any(depths.values()):
            fields.append(body[start:index])
            start = index + 1
        index += 1
    fields.append(body[start:])
    result: dict[str, str] = {}
    for field in fields:
        match = re.match(r"(?s)^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$", field)
        if match is not None:
            result[match.group(1)] = match.group(2)
    return result


def static_rules(root: Path, package_dirs: set[str]) -> dict[str, tuple[str, str]]:
    result: dict[str, tuple[str, str]] = {}
    for package_dir in sorted(package_dirs | {""}):
        build = root / package_dir / "BUILD.bazel"
        if not build.is_file():
            fail(f"missing BUILD.bazel for workspace package {package_dir or '//'}")
        for kind, body in calls(build.read_text(encoding="utf-8")):
            name = call_attributes(body).get("name", "")
            match = re.fullmatch(r'"([A-Za-z0-9._+-]+)"', name)
            if match is None:
                continue
            label = f"//{package_dir}:{match.group(1)}" if package_dir else f"//:{match.group(1)}"
            if label in result:
                fail(f"duplicate static Bazel label {label}")
            result[label] = (kind, body)
    return result


def declared_labels(inventory: dict[str, Any]) -> tuple[dict[str, str], set[str]]:
    owners: dict[str, str] = {}
    production: set[str] = set()
    for package, entry in inventory["packages"].items():
        labels = list(entry["targets"].values())
        labels.extend(entry["focused_tests"])
        labels.extend(entry["bazel_only_targets"])
        if entry["native_unit"] is not None:
            labels.append(entry["native_unit"])
        for target, variants in entry["production_targets"].items():
            for variant in variants:
                labels.append(variant["label"])
                if variant["kind"] == "rust":
                    production.add(variant["label"])
        for label in labels:
            previous = owners.get(label)
            if previous is not None and previous != package:
                fail(f"Bazel label {label} is owned by both {previous} and {package}")
            owners[label] = package
    return owners, production


def cargo_required_features(package: dict[str, Any], target_key: str) -> set[str]:
    kind, name = target_key.split(":", 1)
    if kind != "bin":
        return set()
    for target in package["cargo"].get("bin", []):
        if target.get("name") == name:
            return set(target.get("required-features", []))
    return set()


def bazel_projection(inventory: dict[str, Any]) -> str:
    labels = []
    for entry in inventory["packages"].values():
        parent = Path(entry["manifest"]).parent
        package_dir = "" if parent == Path(".") else parent.as_posix()
        labels.append(f'    "//{package_dir}:cargo_package_data",' if package_dir else '    "//:cargo_package_data",')
    return (
        '"""Generated projection of rust-target-inventory.json; do not edit."""\n\n'
        "CARGO_WORKSPACE_PACKAGE_DATA = [\n"
        + "\n".join(sorted(labels))
        + "\n]\n"
    )


def _production_expression_uses_select(build_text: str, expression: str) -> bool:
    queue = [expression]
    visited: set[str] = set()
    while queue:
        value = queue.pop()
        if "select(" in value:
            return True
        for name in re.findall(r"\b[A-Z][A-Z0-9_]+\b", value):
            if name in visited:
                continue
            visited.add(name)
            match = re.search(
                rf"(?ms)^{re.escape(name)}\s*=\s*(.*?)(?=^[A-Z][A-Z0-9_]*\s*=|^[A-Za-z_][A-Za-z0-9_]*\(|\Z)",
                build_text,
            )
            if match is not None:
                queue.append(match.group(1))
    return False


def check_bazel_rules(root: Path, inventory: dict[str, Any], packages: list[dict[str, Any]]) -> None:
    package_dirs = {package["root"] for package in packages}
    rules = static_rules(root, package_dirs)
    owners, production = declared_labels(inventory)
    static_rust = {label for label, (kind, _body) in rules.items() if kind in RUST_RULES}
    missing = sorted(static_rust - set(owners))
    stale = sorted(label for label in owners if label not in rules)
    if missing or stale:
        fail(f"Bazel target ownership mismatch: missing={missing} stale={stale}")

    testonly_labels: set[str] = set()
    for entry in inventory["packages"].values():
        testonly_labels.update(entry["targets"][key] for key in entry["test_only_targets"])
        for labels in entry["test_only_feature_targets"].values():
            testonly_labels.update(labels)
    for label in sorted(testonly_labels):
        if call_attributes(rules[label][1]).get("testonly") != "True":
            fail(f"test-only Cargo target/feature proof is not testonly=True: {label}")

    for package, entry in inventory["packages"].items():
        for target, label in entry["targets"].items():
            kind = rules[label][0]
            cargo_kind = target.split(":", 1)[0]
            if cargo_kind == "custom-build":
                expected = {"sh_test"}
            elif cargo_kind == "lib":
                cargo_package = next(item for item in packages if item["package"] == package)
                library = cargo_package["cargo"].get("lib", {})
                is_proc_macro = isinstance(library, dict) and library.get("proc-macro") is True
                expected = {"rust_proc_macro"} if is_proc_macro else {"rust_library"}
            elif cargo_kind in {"bin", "example"}:
                expected = {"ctx_rust_binary", "rust_binary"}
            else:
                expected = {"ctx_rust_test", "rust_test", "ctx_cli_integration_test"}
            if kind not in expected:
                fail(f"{package} {target} maps to wrong Bazel rule kind {kind}: {label}")

    # Cross-target rules_rust toolchains are native-only. The action aspect is
    # analyzed in the host configuration, so any future platform-varying source
    # or workspace-dependency attr must fail closed instead of escaping review.
    for label in sorted(production):
        kind, body = rules[label]
        if kind not in RUST_RULES:
            fail(f"production Rust label has non-Rust rule kind {kind}: {label}")
        attributes = call_attributes(body)
        for attr in ("srcs", "deps", "proc_macro_deps", "crate_root", "crate_features"):
            package_dir = label[2:].split(":", 1)[0]
            build_text = (root / package_dir / "BUILD.bazel").read_text(encoding="utf-8")
            if attr in attributes and _production_expression_uses_select(build_text, attributes[attr]):
                fail(f"platform-varying production {attr} is unsupported by the native-only action inventory: {label}")

    projection = root / "tools/bazel/rust-target-inventory.bzl"
    if not projection.is_file() or projection.read_text(encoding="utf-8") != bazel_projection(inventory):
        fail("generated Bazel package-data projection is stale; run the inventory checker with --write-projection")
    for entry in inventory["packages"].values():
        package_dir = Path(entry["manifest"]).parent.as_posix()
        label = f"//{package_dir}:cargo_package_data" if package_dir != "." else "//:cargo_package_data"
        if label not in rules or rules[label][0] != "filegroup":
            fail(f"workspace package lacks crate-specific cargo_package_data: {label}")

    root_build = (root / "BUILD.bazel").read_text(encoding="utf-8")
    gate_calls = [body for kind, body in calls(root_build) if kind == "rust_crate_gate"]
    if len(gate_calls) != 1:
        fail("root BUILD.bazel must declare exactly one rust_crate_gate")
    root_labels = sorted(set(re.findall(r'"(//[^"\s]+:[^"\s]+)"', gate_calls[0])))
    if root_labels != inventory["bazel_roots"]:
        fail(f"configured Bazel roots mismatch: BUILD={root_labels} inventory={inventory['bazel_roots']}")


def _literal_string_list(expression: str, label: str) -> set[str]:
    try:
        value = ast.literal_eval(expression)
    except (SyntaxError, ValueError) as error:
        fail(f"live Bazel {label} is not an exact string list: {expression}: {error}")
    if not isinstance(value, list) or any(not isinstance(item, str) for item in value):
        fail(f"live Bazel {label} is not an exact string list: {expression}")
    return set(value)


def configured_features(attributes: dict[str, str], label: str) -> set[str]:
    result = _literal_string_list(attributes.get("crate_features", "[]"), f"crate_features for {label}")
    rustc_flags = attributes.get("rustc_flags", "[]")
    if "feature=" in rustc_flags:
        if "select(" in rustc_flags:
            fail(f"platform-varying production feature rustc_flags are unsupported: {label}")
        for flag in _literal_string_list(rustc_flags, f"rustc_flags for {label}"):
            match = re.fullmatch(r'--cfg(?:=|\s+)feature=(?:\\?\")([^\"]+)(?:\\?\")', flag)
            if match is not None:
                result.add(match.group(1))
    return result


def _strings(value: Any) -> set[str]:
    if isinstance(value, str):
        return {value}
    if isinstance(value, (list, tuple, set)):
        return set().union(*(_strings(item) for item in value), set())
    if isinstance(value, dict):
        return set().union(*(_strings(item) for item in value.values()), set())
    return set()


def select_values(expression: str, label: str) -> set[str]:
    result: set[str] = set()
    offset = 0
    while True:
        start = expression.find("select(", offset)
        if start < 0:
            return result
        opening = start + len("select")
        end = _call_end(expression, opening)
        try:
            mapping = ast.literal_eval(expression[opening + 1 : end])
        except (SyntaxError, ValueError) as error:
            fail(f"live Bazel select for {label} is not an exact literal mapping: {error}")
        if not isinstance(mapping, dict):
            fail(f"live Bazel select for {label} is not a mapping")
        result.update(_strings(mapping))
        offset = end + 1


def read_live_labels(path: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        match = re.fullmatch(r"([A-Za-z_][A-Za-z0-9_]*) rule (//\S+)", line)
        if match is None:
            fail(f"malformed live Rust label record: {line!r}")
        result[match.group(2)] = match.group(1)
    return result


def read_live_builds(path: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    current: str | None = None
    body: list[str] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("@@LABEL\t"):
            if current is not None:
                fail(f"nested live Bazel build record: {line!r}")
            current = line.split("\t", 1)[1]
            body = []
        elif line == "@@END":
            if current is None or current in result:
                fail("malformed or duplicate live Bazel build record")
            result[current] = "\n".join(body) + "\n"
            current = None
        elif current is not None:
            body.append(line)
        elif line:
            fail(f"content outside live Bazel build record: {line!r}")
    if current is not None:
        fail(f"unterminated live Bazel build record: {current}")
    return result


def check_live_bazel(
    root: Path,
    inventory: dict[str, Any],
    packages: list[dict[str, Any]],
    labels_path: Path,
    builds_path: Path,
) -> None:
    package_dirs = {package["root"] for package in packages}
    rules = static_rules(root, package_dirs)
    owners, production = declared_labels(inventory)
    live = read_live_labels(labels_path)
    missing = sorted(set(live) - set(owners))
    stale = sorted(
        label for label, (kind, _body) in rules.items()
        if label in owners and kind in RUST_RULES and label not in live
    )
    if missing or stale:
        fail(f"live Bazel target ownership mismatch: missing={missing} stale={stale}")
    feature_proofs: dict[str, set[str]] = {}
    for package_name, entry in inventory["packages"].items():
        package = next(package for package in packages if package["package"] == package_name)
        for feature, proof_labels in entry["test_only_feature_targets"].items():
            for proof_label in proof_labels:
                feature_proofs.setdefault(proof_label, set()).add(feature)
        for target_key in entry["test_only_targets"]:
            proof_label = entry["targets"][target_key]
            feature_proofs.setdefault(proof_label, set()).update(
                cargo_required_features(package, target_key)
            )
    expected_builds = production | set(feature_proofs)
    builds = read_live_builds(builds_path)
    if set(builds) != expected_builds:
        fail(f"live Rust build mismatch: missing={sorted(expected_builds-set(builds))} stale={sorted(set(builds)-expected_builds)}")
    variants = {
        variant["label"]: set(variant["features"])
        for entry in inventory["packages"].values()
        for values in entry["production_targets"].values()
        for variant in values
        if variant["kind"] == "rust"
    }
    for label in sorted(production):
        rust_calls = [(kind, body) for kind, body in calls(builds[label]) if kind in RUST_RULES]
        if len(rust_calls) != 1:
            fail(f"live Bazel build must contain one expanded Rust rule: {label}")
        _kind, body = rust_calls[0]
        attributes = call_attributes(body)
        for attr in ("srcs", "deps", "proc_macro_deps", "crate_root", "crate_features"):
            expression = attributes.get(attr, "")
            if "select(" not in expression:
                continue
            if attr in {"deps", "proc_macro_deps"}:
                varying = select_values(expression, f"{attr} for {label}")
                if not any(value.startswith(("//", ":")) for value in varying):
                    continue
            fail(f"live macro-expanded production {attr} varies by configuration: {label}")
        actual_features = configured_features(attributes, label)
        if actual_features != variants[label]:
            fail(f"live Bazel feature mismatch for {label}: inventory={sorted(variants[label])} bazel={sorted(actual_features)}")
    for label, expected_features in sorted(feature_proofs.items()):
        rust_calls = [(kind, body) for kind, body in calls(builds[label]) if kind in RUST_RULES]
        if len(rust_calls) != 1:
            fail(f"live Bazel test-only proof must contain one expanded Rust rule: {label}")
        attributes = call_attributes(rust_calls[0][1])
        if attributes.get("testonly") != "True":
            fail(f"live test-only Cargo target/feature proof is not testonly=True: {label}")
        actual_features = configured_features(attributes, label)
        if actual_features != expected_features:
            fail(
                f"live Bazel test-only feature proof mismatch for {label}: "
                f"inventory={sorted(expected_features)} bazel={sorted(actual_features)}"
            )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inventory")
    parser.add_argument("root_cargo_toml")
    parser.add_argument("--live-labels")
    parser.add_argument("--live-builds")
    parser.add_argument("--write-projection", action="store_true")
    args = parser.parse_args()
    inventory_path = Path(args.inventory).resolve()
    root_manifest = Path(args.root_cargo_toml).resolve()
    root = root_manifest.parent
    gate = load_gate(root)
    try:
        inventory = json.loads(inventory_path.read_text(encoding="utf-8"))
        if args.write_projection:
            (root / "tools/bazel/rust-target-inventory.bzl").write_text(bazel_projection(inventory), encoding="utf-8")
        paths = {
            path.relative_to(root).as_posix()
            for path in root.rglob("*")
            if path.is_file()
        }
        view = gate.SourceView(root, paths)
        packages = gate.workspace_packages(view)
        gate.validate_inventory_packages(view, packages, inventory)
        check_bazel_rules(root, inventory, packages)
        if bool(args.live_labels) != bool(args.live_builds):
            fail("--live-labels and --live-builds must be supplied together")
        if args.live_labels:
            check_live_bazel(root, inventory, packages, Path(args.live_labels), Path(args.live_builds))
    except (OSError, UnicodeError, json.JSONDecodeError, gate.GateError) as error:
        fail(str(error))
    print(
        f"native Bazel inventory owns complete metadata for {len(packages)} workspace packages "
        f"and {sum(len(value['targets']) for value in inventory['packages'].values())} Cargo targets"
    )


if __name__ == "__main__":
    main()
