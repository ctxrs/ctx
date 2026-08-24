#!/usr/bin/env python3
"""Validate the canonical config/environment control surface."""

from __future__ import annotations

import json
import hashlib
import re
import subprocess
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
RELEASED_DEFAULT_SCOPES = {
    "analytics.enabled": "all_cli_installations",
    "local_usage.enabled": "all_cli_installations",
    "upgrade.auto": "official_installer_managed",
    "indexing.mode": "all_cli_installations",
    "search.semantic": "all_cli_installations",
}
PINNED_STABLE_SNAPSHOTS = {
    "v0.25.0": {
        "path": "contracts/stable-defaults/v0.25.0.json",
        "sha256": "3a6d7565ca729e50864b1ab81eecefa39bf13c643b3b54e0a58dc0915c2694d0",
    },
}
PREVIOUS_STABLE_DEFAULT_KEYS = {
    "analytics.enabled",
    "upgrade.auto",
    "daemon.enabled",
    "search.semantic",
}


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
                and not SKIP_PARTS.intersection(path.relative_to(root).parts)
            ):
                files.append(path)
    return files


def scalar_value(token: str, constants: dict[str, object]) -> object:
    token = token.strip().removesuffix(".to_owned()")
    if token in constants:
        return constants[token]
    if token in {"true", "false"}:
        return token == "true"
    if token.startswith('"') and token.endswith('"'):
        return json.loads(token)
    fail(f"could not resolve empty-config default expression {token!r}")


def extract_empty_config_defaults(config_source: str) -> dict[str, object]:
    constants = {
        name: scalar_value(value, {})
        for name, value in re.findall(
            r'^pub const ([A-Z][A-Z0-9_]+):\s*(?:&str|bool)\s*=\s*(".*?"|true|false);',
            config_source,
            re.MULTILINE,
        )
    }
    default_source = config_source.partition("impl Default for AppConfig")[2].partition(
        "impl AppConfig"
    )[0]
    if not default_source:
        fail("could not locate AppConfig::default")

    def default_field(pattern: str, label: str) -> object:
        match = re.search(pattern, default_source, re.DOTALL)
        if not match:
            fail(f"could not locate empty-config {label} default")
        return scalar_value(match.group(1), constants)

    semantic = re.search(
        r"pub fn semantic_search_enabled.*?unwrap_or\(([^)]+)\)",
        config_source,
        re.DOTALL,
    )
    if not semantic:
        fail("could not locate empty-config semantic search default")
    defaults = {
        "analytics.enabled": default_field(
            r"analytics:\s*AnalyticsConfig\s*\{.*?enabled:\s*([^,\n]+),",
            "analytics",
        ),
        "upgrade.auto": default_field(
            r"upgrade:\s*UpgradeConfig\s*\{.*?auto:\s*([^,\n]+),",
            "automatic upgrade",
        ),
        "search.semantic": scalar_value(semantic.group(1), constants),
    }
    indexing_mode = re.search(
        r"indexing:\s*IndexingConfig\s*\{.*?mode:\s*IndexingMode::([A-Za-z]+),",
        default_source,
        re.DOTALL,
    )
    if indexing_mode:
        mode_variant = indexing_mode.group(1)
        defaults["indexing.mode"] = {
            "Automatic": "auto",
            "Manual": "manual",
        }.get(mode_variant, mode_variant.lower())
    else:
        # Previous stable releases exposed the same control as a boolean.
        # Keep extracting that historical key so the pinned snapshot can be
        # verified before main() maps it to the canonical mode vocabulary.
        defaults["daemon.enabled"] = default_field(
            r"daemon:\s*DaemonConfig\s*\{.*?enabled:\s*([^,\n]+),",
            "daemon",
        )
    if "LocalUsageConfig" in default_source:
        defaults["local_usage.enabled"] = default_field(
            r"local_usage:\s*LocalUsageConfig\s*\{.*?enabled:\s*([^,\n]+),",
            "local usage",
        )
    return defaults


def default_state(value: object) -> str:
    return "on" if value is True or value in {"apply", "auto", "automatic"} else "off"


def previous_stable_defaults(
    root: Path, tag: str, config_relative: str, snapshot_relative: str
) -> dict[str, object]:
    pinned = PINNED_STABLE_SNAPSHOTS.get(tag)
    if pinned is None or snapshot_relative != pinned["path"]:
        fail(f"previous stable snapshot is not pinned for {tag}")
    snapshot_path = root / snapshot_relative
    snapshot_bytes = snapshot_path.read_bytes()
    snapshot_digest = hashlib.sha256(snapshot_bytes).hexdigest()
    if snapshot_digest != pinned["sha256"]:
        fail(
            f"pinned previous stable snapshot digest differs for {tag}: "
            f"expected={pinned['sha256']} actual={snapshot_digest}"
        )
    snapshot = json.loads(snapshot_bytes)
    expected_keys = PREVIOUS_STABLE_DEFAULT_KEYS
    defaults = snapshot.get("defaults")
    if (
        snapshot.get("schema_version") != 1
        or snapshot.get("release_tag") != tag
        or snapshot.get("config_source") != config_relative
        or not isinstance(snapshot.get("config_source_sha256"), str)
        or re.fullmatch(r"[0-9a-f]{64}", snapshot["config_source_sha256"]) is None
        or not isinstance(defaults, dict)
        or set(defaults) != expected_keys
    ):
        fail(f"invalid previous stable snapshot {snapshot_relative}")

    try:
        probe = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "--git-dir"],
            check=False,
            capture_output=True,
            text=True,
        )
    except FileNotFoundError:
        probe = None
    if probe is None or probe.returncode != 0:
        return defaults
    source = subprocess.run(
        ["git", "-C", str(root), "show", f"{tag}:{config_relative}"],
        check=False,
        capture_output=True,
    )
    if source.returncode != 0:
        fail(
            f"could not read previous stable defaults from {tag}: "
            f"{source.stderr.decode(errors='replace').strip()}"
        )
    digest = hashlib.sha256(source.stdout).hexdigest()
    if digest != snapshot["config_source_sha256"]:
        fail(
            f"previous stable snapshot source digest differs from {tag}: "
            f"snapshot={snapshot['config_source_sha256']} stable={digest}"
        )
    extracted = extract_empty_config_defaults(source.stdout.decode())
    if extracted != defaults:
        fail(
            f"previous stable snapshot defaults differ from {tag}: "
            f"snapshot={defaults!r} stable={extracted!r}"
        )
    return defaults


def validate_released_defaults(
    controls: list[dict[str, object]],
    runtime_defaults: dict[str, object],
    previous_defaults: dict[str, object] | None,
    previous_tag: str,
) -> None:
    controls_by_key = {control["config_key"]: control for control in controls}
    declared = {
        key for key, control in controls_by_key.items() if "released_default" in control
    }
    if declared != set(RELEASED_DEFAULT_SCOPES):
        fail(
            "released-default controls differ from runtime inventory: "
            f"declared={sorted(declared)} expected={sorted(RELEASED_DEFAULT_SCOPES)}"
        )

    for config_key, expected_scope in RELEASED_DEFAULT_SCOPES.items():
        control = controls_by_key[config_key]
        behavior = control["behavior"]
        released = control.get("released_default")
        previous = control.get("previous_stable_default")
        expected_value = runtime_defaults[config_key]
        if not isinstance(released, dict) or released != {
            "value": expected_value,
            "state": default_state(expected_value),
            "scope": expected_scope,
        }:
            fail(
                f"{behavior} released default differs from empty-config runtime: "
                f"declared={released!r} runtime={expected_value!r}"
            )
        if config_key not in previous_defaults:
            if previous is not None:
                fail(f"{behavior} declares a previous default before it existed")
            if control.get("introduced_after") != previous_tag:
                fail(f"{behavior} must declare introduced_after={previous_tag}")
            if control.get("deliberate_change_approval") is not None:
                fail(f"{behavior} has change approval despite being newly introduced")
            continue

        expected_previous = previous_defaults[config_key]
        if not isinstance(previous, dict) or previous != {
            "value": expected_previous,
            "state": default_state(expected_previous),
        }:
            fail(
                f"{behavior} previous stable default differs from {previous_tag}: "
                f"declared={previous!r} stable={expected_previous!r}"
            )

        changed = previous["value"] != released["value"]
        approval = control.get("deliberate_change_approval")
        if changed:
            if not isinstance(approval, dict):
                fail(f"{behavior} changed default lacks deliberate-change approval")
            evidence = approval.get("evidence_commits")
            if (
                approval.get("previous_stable_tag") != previous_tag
                or not isinstance(evidence, list)
                or not evidence
                or any(
                    not isinstance(commit, str)
                    or re.fullmatch(r"[0-9a-f]{40}", commit) is None
                    for commit in evidence
                )
            ):
                fail(f"{behavior} deliberate-change approval lacks scoped commit evidence")
            if not isinstance(approval.get("reason"), str) or not approval["reason"].strip():
                fail(f"{behavior} deliberate-change approval lacks a reason")
        elif approval is not None:
            fail(f"{behavior} has a generic approval despite no default change")


def main() -> None:
    if len(sys.argv) > 2:
        fail("usage: check-public-control-surface.py [repository-root]")
    root = (
        Path(sys.argv[1]).resolve()
        if len(sys.argv) == 2
        else Path(__file__).resolve().parent.parent
    )
    contract_path = root / "contracts" / "public-control-surface-v1.json"
    config_path = root / "crates" / "ctx-cli" / "src" / "config.rs"
    contract = json.loads(contract_path.read_text(encoding="utf-8"))
    if contract.get("schema_version") != 1:
        fail("unsupported contract schema")
    previous_tag = contract.get("previous_stable_tag")
    if not isinstance(previous_tag, str) or not re.fullmatch(
        r"v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)",
        previous_tag,
    ):
        fail("previous_stable_tag must name an exact stable SemVer tag")

    controls = contract.get("controls")
    if not isinstance(controls, list) or not controls:
        fail("controls must be a non-empty list")
    behaviors = [control["behavior"] for control in controls]
    config_keys = [control["config_key"] for control in controls]
    env_vars = [control["environment_variable"] for control in controls]
    unique(behaviors, "behaviors")
    unique(config_keys, "config keys")
    unique(env_vars, "environment variables")
    rejected_config_keys = contract.get("rejected_config_keys")
    if (
        not isinstance(rejected_config_keys, list)
        or not rejected_config_keys
        or any(not isinstance(key, str) or not key for key in rejected_config_keys)
    ):
        fail("rejected_config_keys must be a non-empty string list")
    unique(rejected_config_keys, "rejected config keys")
    overlap = sorted(set(config_keys).intersection(rejected_config_keys))
    if overlap:
        fail("rejected config keys overlap canonical controls: " + ", ".join(overlap))

    config_source = config_path.read_text(encoding="utf-8")
    runtime_defaults = extract_empty_config_defaults(config_source)
    stable_defaults = previous_stable_defaults(
        root,
        previous_tag,
        "crates/ctx-cli/src/config.rs",
        contract.get("previous_stable_snapshot", ""),
    )
    stable_defaults["indexing.mode"] = (
        "automatic" if stable_defaults.pop("daemon.enabled") else "manual"
    )
    validate_released_defaults(
        controls, runtime_defaults, stable_defaults, previous_tag
    )
    _, values_separator, values_and_env = config_source.partition("    fn apply_values")
    if not values_separator:
        fail("could not locate AppConfig::apply_values")
    apply_values, separator, apply_env = values_and_env.partition("    fn apply_env")
    if not separator:
        fail("could not locate AppConfig::apply_env")
    implemented_keys = set(
        re.findall(r'^\s+"([a-z][a-z0-9_.]+)"\s*=>', apply_values, re.MULTILINE)
    )
    # Canonical controls may be owned by a purpose-specific helper called from
    # apply_env. Scan the complete production config module so helper-owned
    # variables are inventoried while any undocumented literal still fails.
    implemented_env = set(re.findall(r'"(CTX_[A-Z0-9_]+)"', config_source))
    compatibility_config_keys = contract.get("compatibility_config_keys")
    if (
        not isinstance(compatibility_config_keys, dict)
        or any(
            not isinstance(key, str)
            or not isinstance(target, str)
            or target not in config_keys
            or key in config_keys
            for key, target in compatibility_config_keys.items()
        )
    ):
        fail("compatibility_config_keys must map legacy keys to canonical controls")
    contract_keys = (
        set(config_keys)
        .union(rejected_config_keys)
        .union(compatibility_config_keys)
    )
    if implemented_keys != contract_keys:
        fail(
            "config keys differ from contract: "
            f"implemented={sorted(implemented_keys)} contract={sorted(contract_keys)}"
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
    retired_containment_paths: set[Path] = set()
    for containment in contract.get("retired_control_containment", []):
        path = root / containment["path"]
        purpose = containment.get("purpose")
        declared_controls = containment.get("controls")
        if (
            path in retired_containment_paths
            or not path.is_file()
            or not isinstance(purpose, str)
            or not purpose.strip()
            or not isinstance(declared_controls, list)
            or not declared_controls
            or len(set(declared_controls)) != len(declared_controls)
            or not set(declared_controls).issubset(retired)
        ):
            fail(f"invalid retired control containment: {containment!r}")
        text = path.read_text(encoding="utf-8")
        present = sorted(control for control in retired if control in text)
        if present != sorted(declared_controls):
            fail(
                f"retired control containment {containment['path']} differs from inventory: "
                f"implemented={present} declared={sorted(declared_controls)}"
            )
        retired_containment_paths.add(path)
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
    consumer_paths: set[Path] = set()
    for consumer in contract.get("deprecated_compatibility_consumers", []):
        path = root / consumer["path"]
        purpose = consumer.get("purpose")
        declared_controls = consumer.get("controls")
        if (
            path in consumer_paths
            or not isinstance(purpose, str)
            or not purpose.strip()
            or not isinstance(declared_controls, list)
            or not declared_controls
            or len(set(declared_controls)) != len(declared_controls)
            or not set(declared_controls).issubset(deprecated)
        ):
            fail(f"invalid deprecated compatibility consumer: {consumer!r}")
        text = path.read_text(encoding="utf-8")
        present = sorted(control for control in deprecated if control in text)
        if present != sorted(declared_controls):
            fail(
                f"deprecated compatibility consumer {consumer['path']} differs from inventory: "
                f"implemented={present} declared={sorted(declared_controls)}"
            )
        consumer_paths.add(path)
    violations: list[str] = []
    for path in tracked_text_files(
        root, {contract_path, retired_reference, *retired_containment_paths}
    ):
        text = path.read_text(encoding="utf-8", errors="replace")
        for control in retired:
            if control in text:
                violations.append(f"{path.relative_to(root)}: retired control {control}")
    if violations:
        fail("\n  ".join(["retired controls remain:", *sorted(violations)]))

    deprecated_violations: list[str] = []
    excluded = {
        contract_path,
        compatibility_handler,
        *deprecated_references,
        *consumer_paths,
    }
    for path in tracked_text_files(root, excluded):
        relative = path.relative_to(root)
        is_test = (
            "tests" in relative.parts
            or relative.name.endswith("_tests.rs")
            or relative.name.startswith("test-")
        )
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
        f"{len(retired)} retired controls contained, "
        f"{len(runtime_defaults)} empty-config released defaults"
    )


if __name__ == "__main__":
    main()
