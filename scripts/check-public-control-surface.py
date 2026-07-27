#!/usr/bin/env python3
"""Validate the canonical config/environment control surface."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path


TEXT_SUFFIXES = {
    ".bazel",
    ".bzl",
    ".cjs",
    ".json",
    ".md",
    ".mjs",
    ".ps1",
    ".py",
    ".rs",
    ".sh",
    ".toml",
    ".yaml",
    ".yml",
}
SKIP_PARTS = {".git", "bazel-bin", "bazel-out", "bazel-testlogs", "target"}


def fail(message: str) -> None:
    raise SystemExit(f"public control surface check failed: {message}")


def unique(values: list[str], label: str) -> None:
    duplicates = sorted({value for value in values if values.count(value) > 1})
    if duplicates:
        fail(f"duplicate {label}: {', '.join(duplicates)}")


def tracked_text_files(root: Path, excluded: set[Path]) -> list[Path]:
    files: list[Path] = []
    for relative in (
        ".buildkite",
        "BUILD.bazel",
        "MODULE.bazel",
        "README.md",
        "SECURITY.md",
        "contracts",
        "crates",
        "docs",
        "plugins",
        "protocol",
        "scripts",
        "sdks",
        "skills",
        "tests",
    ):
        candidate = root / relative
        paths = [candidate] if candidate.is_file() else candidate.rglob("*")
        for path in paths:
            if (
                path.is_file()
                and path not in excluded
                and path.suffix in TEXT_SUFFIXES
                and not SKIP_PARTS.intersection(path.parts)
            ):
                files.append(path)
    return files


def main() -> None:
    root = Path(__file__).resolve().parent.parent
    contract_path = root / "contracts" / "public-control-surface-v1.json"
    config_path = root / "crates" / "ctx-cli" / "src" / "config.rs"
    contract = json.loads(contract_path.read_text(encoding="utf-8"))
    if contract.get("schema_version") != 1:
        fail("unsupported contract schema")

    controls = contract.get("controls")
    if not isinstance(controls, list) or not controls:
        fail("controls must be a non-empty list")
    behaviors = [control["behavior"] for control in controls]
    config_keys = [control["config_key"] for control in controls]
    env_vars = [control["environment_variable"] for control in controls]
    unique(behaviors, "behaviors")
    unique(config_keys, "config keys")
    unique(env_vars, "environment variables")

    config_source = config_path.read_text(encoding="utf-8")
    _, values_separator, values_and_env = config_source.partition("    fn apply_values")
    if not values_separator:
        fail("could not locate AppConfig::apply_values")
    apply_values, separator, apply_env = values_and_env.partition("    fn apply_env")
    if not separator:
        fail("could not locate AppConfig::apply_env")
    implemented_keys = set(
        re.findall(r'^\s+"([a-z][a-z0-9_.]+)"\s*=>', apply_values, re.MULTILINE)
    )
    implemented_env = set(re.findall(r'"(CTX_[A-Z0-9_]+)"', apply_env.split("\n    pub fn", 1)[0]))
    if implemented_keys != set(config_keys):
        fail(
            "config keys differ from contract: "
            f"implemented={sorted(implemented_keys)} contract={sorted(config_keys)}"
        )
    if implemented_env != set(env_vars):
        fail(
            "config environment variables differ from contract: "
            f"implemented={sorted(implemented_env)} contract={sorted(env_vars)}"
        )

    retired = contract.get("retired_controls")
    if not isinstance(retired, list):
        fail("retired_controls must be a list")
    unique(retired, "retired controls")
    retired_reference = root / contract["retired_control_reference"]
    reference_text = retired_reference.read_text(encoding="utf-8")
    missing_references = [control for control in retired if control not in reference_text]
    if missing_references:
        fail(
            "retired control migration reference is incomplete: "
            + ", ".join(missing_references)
        )
    compatibility_handler = root / contract["deprecated_compatibility_handler"]
    compatibility_source = compatibility_handler.read_text(encoding="utf-8")
    deprecated = re.findall(r'^\s+name:\s+"(CTX_[A-Z0-9_]+)",', compatibility_source, re.MULTILINE)
    unique(deprecated, "deprecated compatibility controls")
    if len(deprecated) != contract["deprecated_compatibility_count"]:
        fail(
            "deprecated compatibility registry count differs from contract: "
            f"implemented={len(deprecated)} contract={contract['deprecated_compatibility_count']}"
        )
    if "deprecated_compatibility_removal_version" not in contract:
        fail("deprecated compatibility removal policy is missing")
    if contract["deprecated_compatibility_removal_version"] is not None:
        fail("deprecated compatibility aliases must not promise a removal version")
    overlap = sorted(set(deprecated).intersection(env_vars).union(set(deprecated).intersection(retired)))
    if overlap:
        fail("deprecated compatibility controls overlap canonical or retired controls: " + ", ".join(overlap))
    deprecated_references = {
        root / relative for relative in contract["deprecated_control_references"]
    }
    for reference in deprecated_references:
        reference_text = reference.read_text(encoding="utf-8")
        missing = [control for control in deprecated if control not in reference_text]
        if missing:
            fail(
                f"deprecated compatibility reference {reference.relative_to(root)} is incomplete: "
                + ", ".join(missing)
            )
    violations: list[str] = []
    for path in tracked_text_files(root, {contract_path, retired_reference}):
        text = path.read_text(encoding="utf-8", errors="replace")
        for control in retired:
            if control in text:
                violations.append(f"{path.relative_to(root)}: retired control {control}")
    if violations:
        fail("\n  ".join(["retired controls remain:", *sorted(violations)]))

    deprecated_violations: list[str] = []
    excluded = {contract_path, compatibility_handler, *deprecated_references}
    for path in tracked_text_files(root, excluded):
        relative = path.relative_to(root)
        is_test = "tests" in relative.parts or relative.name.endswith("_tests.rs")
        if is_test:
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        for control in deprecated:
            if control in text:
                deprecated_violations.append(f"{relative}: deprecated control {control}")
    if deprecated_violations:
        fail(
            "\n  ".join(
                [
                    "deprecated controls escaped their handler, tests, or migration notes:",
                    *sorted(deprecated_violations),
                ]
            )
        )

    print(
        "public control surface check passed: "
        f"{len(controls)} behaviors, {len(deprecated)} deprecated compatibility controls, "
        f"{len(retired)} retired controls absent"
    )


if __name__ == "__main__":
    main()
