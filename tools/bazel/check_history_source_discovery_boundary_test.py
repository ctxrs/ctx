#!/usr/bin/env python3

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from check_history_source_discovery_boundary import (
    BoundaryError,
    EXPECTED_INTERNAL_BAZEL,
    validate_bazel_inventory,
    validate_discovery_environment_sources,
    validate_manifest,
)


CARGO = """\
[package]
name = "ctx-history-source-discovery"
version = "0.0.0"
edition = "2021"

[dependencies]
chrono.workspace = true
directories.workspace = true
json5.workspace = true
jsonc-parser.workspace = true
libc.workspace = true
quick-xml.workspace = true
rusqlite.workspace = true
same-file = "1.0.6"
serde_json.workspace = true
serde_yaml.workspace = true
sha2.workspace = true
thiserror.workspace = true
toml_edit.workspace = true
ctx-history-capture-model = { path = "../ctx-history-capture-model" }
ctx-history-core = { path = "../ctx-history-core" }
ctx-history-openclaw-schema = { path = "../ctx-history-openclaw-schema" }
ctx-history-platform = { path = "../ctx-history-platform" }
ctx-history-source-io = { path = "../ctx-history-source-io" }
ctx-history-source-sqlite = { path = "../ctx-history-source-sqlite" }

[dev-dependencies]
ctx-history-openclaw-schema = { path = "../ctx-history-openclaw-schema", features = ["test-support"] }
tempfile.workspace = true
ctx-history-source-io = { path = "../ctx-history-source-io", features = ["test-support"] }
ctx-history-source-sqlite = { path = "../ctx-history-source-sqlite", features = ["test-support"] }
"""

SUPERVISOR_ENVIRONMENT = """\
const DISCOVERY_ENV_ALLOWLIST: &[&str] = &[
    "APPDATA",
    "CODEX_HOME",
    "XDG_DATA_HOME",
];
"""

CANONICAL_ENVIRONMENT = """\
pub const DISCOVERY_ENV_ALLOWLIST: &[&str] = &[
    "APPDATA",
    "CODEX_HOME",
    "XDG_DATA_HOME",
];
"""


class BoundaryMutationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.manifest = Path(self.temporary.name) / "Cargo.toml"
        self.manifest.write_text(CARGO, encoding="utf-8")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_current_cargo_and_queried_bazel_inventories_pass(self) -> None:
        validate_manifest(self.manifest)
        validate_bazel_inventory(EXPECTED_INTERNAL_BAZEL, "direct")

    def test_cargo_only_forbidden_edge_is_rejected(self) -> None:
        self.manifest.write_text(
            self.manifest.read_text(encoding="utf-8").replace(
                "ctx-history-core =",
                'ctx-history-jsonl = { path = "../ctx-history-jsonl" }\nctx-history-core =',
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "Cargo production dependency"):
            validate_manifest(self.manifest)
        validate_bazel_inventory(EXPECTED_INTERNAL_BAZEL, "direct")

    def test_bazel_only_forbidden_edge_is_rejected(self) -> None:
        validate_manifest(self.manifest)
        queried = EXPECTED_INTERNAL_BAZEL | {"//crates/ctx-history-capture:lib"}
        with self.assertRaisesRegex(BoundaryError, "Bazel direct internal allowlist"):
            validate_bazel_inventory(queried, "direct")

    def test_unapproved_internal_crate_is_rejected_without_name_denylist(self) -> None:
        queried = EXPECTED_INTERNAL_BAZEL | {"//crates/ctx-protocol:lib"}
        with self.assertRaises(BoundaryError):
            validate_bazel_inventory(queried, "transitive")

    def test_production_source_io_test_support_is_rejected(self) -> None:
        self.manifest.write_text(
            self.manifest.read_text(encoding="utf-8").replace(
                'ctx-history-source-io = { path = "../ctx-history-source-io" }',
                'ctx-history-source-io = { path = "../ctx-history-source-io", features = ["test-support"] }',
                1,
            ),
            encoding="utf-8",
        )
        with self.assertRaises(BoundaryError):
            validate_manifest(self.manifest)

    def test_production_source_sqlite_test_support_is_rejected(self) -> None:
        self.manifest.write_text(
            self.manifest.read_text(encoding="utf-8").replace(
                'ctx-history-source-sqlite = { path = "../ctx-history-source-sqlite" }',
                'ctx-history-source-sqlite = { path = "../ctx-history-source-sqlite", features = ["test-support"] }',
                1,
            ),
            encoding="utf-8",
        )
        with self.assertRaises(BoundaryError):
            validate_manifest(self.manifest)

    def test_matching_ordered_discovery_environment_authorities_pass(self) -> None:
        validate_discovery_environment_sources(
            SUPERVISOR_ENVIRONMENT,
            CANONICAL_ENVIRONMENT,
        )

    def test_supervisor_discovery_environment_addition_is_rejected(self) -> None:
        mutated = SUPERVISOR_ENVIRONMENT.replace(
            '    "CODEX_HOME",\n',
            '    "CLAUDE_CONFIG_DIR",\n    "CODEX_HOME",\n',
        )
        with self.assertRaisesRegex(BoundaryError, "set drifted.*CLAUDE_CONFIG_DIR"):
            validate_discovery_environment_sources(mutated, CANONICAL_ENVIRONMENT)

    def test_supervisor_discovery_environment_removal_is_rejected(self) -> None:
        mutated = SUPERVISOR_ENVIRONMENT.replace('    "CODEX_HOME",\n', "")
        with self.assertRaisesRegex(BoundaryError, "set drifted.*CODEX_HOME"):
            validate_discovery_environment_sources(mutated, CANONICAL_ENVIRONMENT)

    def test_supervisor_discovery_environment_reordering_is_rejected(self) -> None:
        mutated = SUPERVISOR_ENVIRONMENT.replace(
            '    "APPDATA",\n    "CODEX_HOME",\n',
            '    "CODEX_HOME",\n    "APPDATA",\n',
        )
        with self.assertRaisesRegex(BoundaryError, "order drifted.*index=0"):
            validate_discovery_environment_sources(mutated, CANONICAL_ENVIRONMENT)

    def test_duplicate_in_either_discovery_environment_authority_is_rejected(self) -> None:
        for authority, supervisor, canonical in [
            (
                "supervisor",
                SUPERVISOR_ENVIRONMENT.replace(
                    '    "CODEX_HOME",\n',
                    '    "CODEX_HOME",\n    "CODEX_HOME",\n',
                ),
                CANONICAL_ENVIRONMENT,
            ),
            (
                "canonical policy",
                SUPERVISOR_ENVIRONMENT,
                CANONICAL_ENVIRONMENT.replace(
                    '    "CODEX_HOME",\n',
                    '    "CODEX_HOME",\n    "CODEX_HOME",\n',
                ),
            ),
        ]:
            with self.subTest(authority=authority):
                with self.assertRaisesRegex(
                    BoundaryError,
                    f"{authority}.*duplicate.*CODEX_HOME",
                ):
                    validate_discovery_environment_sources(supervisor, canonical)


if __name__ == "__main__":
    unittest.main()
