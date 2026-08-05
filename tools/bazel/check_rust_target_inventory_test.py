#!/usr/bin/env python3

from __future__ import annotations

import json
from pathlib import Path
import tempfile
import unittest

import check_rust_target_inventory as checker


def minimal_inventory(*, bazel_only: list[str] | None = None) -> dict:
    return {
        "schema_version": 2,
        "bazel_roots": ["//crate:lib"],
        "packages": {
            "crate": {
                "manifest": "crate/Cargo.toml",
                "targets": {"lib:crate": "//crate:lib"},
                "production_targets": {
                    "lib:crate": [
                        {"features": [], "kind": "rust", "label": "//crate:lib"}
                    ]
                },
                "test_only_targets": [],
                "production_features": [],
                "test_only_features": [],
                "test_only_feature_targets": {},
                "out_dir_sources": {},
                "native_unit": None,
                "focused_tests": [],
                "bazel_only_targets": bazel_only or [],
            }
        },
    }


class RustTargetInventoryCheckerTest(unittest.TestCase):
    def fixture(self, build: str) -> tuple[tempfile.TemporaryDirectory[str], Path, list[dict]]:
        temporary = tempfile.TemporaryDirectory()
        root = Path(temporary.name)
        (root / "crate").mkdir()
        (root / "crate/BUILD.bazel").write_text(
            'filegroup(name = "cargo_package_data", srcs = ["Cargo.toml"])\n' + build,
            encoding="utf-8",
        )
        (root / "tools/bazel").mkdir(parents=True)
        (root / "tools/bazel/rust-target-inventory.bzl").write_text(
            checker.bazel_projection(minimal_inventory()),
            encoding="utf-8",
        )
        (root / "BUILD.bazel").write_text(
            'RUST_CRATE_GATE_RECORDS = rust_crate_gate(\n'
            '    name = "records",\n'
            '    roots = ["//crate:lib"],\n'
            ')\n',
            encoding="utf-8",
        )
        packages = [{"package": "crate", "root": "crate", "cargo": {"lib": {}}}]
        return temporary, root, packages

    def live_files(self, root: Path, labels: list[str], builds: dict[str, str]) -> tuple[Path, Path]:
        labels_path = root / "live-labels.txt"
        labels_path.write_text(
            "".join(f"rust_library rule {label}\n" for label in labels),
            encoding="utf-8",
        )
        builds_path = root / "live-builds.txt"
        builds_path.write_text(
            "".join(f"@@LABEL\t{label}\n{body}@@END\n" for label, body in builds.items()),
            encoding="utf-8",
        )
        return labels_path, builds_path

    def test_call_parser_handles_nested_expressions_and_assignment(self) -> None:
        parsed = checker.calls(
            'VALUE = rust_crate_gate(\n'
            '  name = "records",\n'
            '  roots = select({"//conditions:default": ["//crate:lib"]}),\n'
            ')\n'
        )
        self.assertEqual([name for name, _body in parsed], ["rust_crate_gate"])

    def test_unowned_bazel_only_rust_target_fails(self) -> None:
        temporary, root, packages = self.fixture(
            'rust_library(name = "lib", srcs = ["lib.rs"])\n'
            'ctx_rust_test(name = "bazel_only", srcs = ["lib.rs"])\n'
        )
        self.addCleanup(temporary.cleanup)
        with self.assertRaisesRegex(SystemExit, "missing=.*bazel_only"):
            checker.check_bazel_rules(root, minimal_inventory(), packages)

    def test_exact_bazel_only_record_passes(self) -> None:
        temporary, root, packages = self.fixture(
            'rust_library(name = "lib", srcs = ["lib.rs"])\n'
            'ctx_rust_test(name = "bazel_only", srcs = ["lib.rs"])\n'
        )
        self.addCleanup(temporary.cleanup)
        checker.check_bazel_rules(
            root,
            minimal_inventory(bazel_only=["//crate:bazel_only"]),
            packages,
        )

    def test_cargo_proc_macro_library_maps_to_rust_proc_macro(self) -> None:
        temporary, root, packages = self.fixture(
            'rust_proc_macro(name = "lib", srcs = ["lib.rs"])\n'
        )
        self.addCleanup(temporary.cleanup)
        packages[0]["cargo"]["lib"]["proc-macro"] = True
        checker.check_bazel_rules(root, minimal_inventory(), packages)

    def test_platform_select_in_production_sources_fails_closed(self) -> None:
        temporary, root, packages = self.fixture(
            'rust_library(\n'
            '    name = "lib",\n'
            '    srcs = select({"//conditions:default": ["lib.rs"]}),\n'
            ')\n'
        )
        self.addCleanup(temporary.cleanup)
        with self.assertRaisesRegex(SystemExit, "platform-varying production srcs"):
            checker.check_bazel_rules(root, minimal_inventory(), packages)

    def test_platform_select_hidden_behind_source_variable_fails_closed(self) -> None:
        temporary, root, packages = self.fixture(
            'PROD_SRCS = select({"//conditions:default": ["lib.rs"]})\n'
            'rust_library(name = "lib", srcs = PROD_SRCS)\n'
        )
        self.addCleanup(temporary.cleanup)
        with self.assertRaisesRegex(SystemExit, "platform-varying production srcs"):
            checker.check_bazel_rules(root, minimal_inventory(), packages)

    def test_live_query_catches_disconnected_macro_generated_rust_target(self) -> None:
        temporary, root, packages = self.fixture('rust_library(name = "lib", srcs = ["lib.rs"])\n')
        self.addCleanup(temporary.cleanup)
        labels, builds = self.live_files(
            root,
            ["//crate:lib", "//other:disconnected"],
            {"//crate:lib": 'rust_library(name = "lib", srcs = ["//crate:lib.rs"])\n'},
        )
        with self.assertRaisesRegex(SystemExit, "live Bazel target ownership mismatch.*disconnected"):
            checker.check_live_bazel(root, minimal_inventory(), packages, labels, builds)

    def test_live_macro_expansion_catches_hidden_source_select(self) -> None:
        temporary, root, packages = self.fixture('rust_library(name = "lib", srcs = ["lib.rs"])\n')
        self.addCleanup(temporary.cleanup)
        labels, builds = self.live_files(
            root,
            ["//crate:lib"],
            {
                "//crate:lib": (
                    'rust_library(name = "lib", '
                    'srcs = select({"//conditions:default": ["//crate:lib.rs"]}))\n'
                )
            },
        )
        with self.assertRaisesRegex(SystemExit, "macro-expanded production srcs varies"):
            checker.check_live_bazel(root, minimal_inventory(), packages, labels, builds)

    def test_live_configured_feature_must_match_exact_variant(self) -> None:
        temporary, root, packages = self.fixture('rust_library(name = "lib", srcs = ["lib.rs"])\n')
        self.addCleanup(temporary.cleanup)
        labels, builds = self.live_files(
            root,
            ["//crate:lib"],
            {
                "//crate:lib": (
                    'rust_library(name = "lib", srcs = ["//crate:lib.rs"], '
                    'rustc_flags = ["--cfg=feature=\\\"hidden\\\""])\n'
                )
            },
        )
        with self.assertRaisesRegex(SystemExit, "live Bazel feature mismatch"):
            checker.check_live_bazel(root, minimal_inventory(), packages, labels, builds)

    def test_test_only_feature_proof_requires_testonly_rule(self) -> None:
        temporary, root, packages = self.fixture(
            'rust_library(name = "lib", srcs = ["lib.rs"])\n'
            'rust_library(name = "support", srcs = ["lib.rs"])\n'
        )
        self.addCleanup(temporary.cleanup)
        inventory = minimal_inventory(bazel_only=["//crate:support"])
        entry = inventory["packages"]["crate"]
        entry["test_only_features"] = ["support"]
        entry["test_only_feature_targets"] = {"support": ["//crate:support"]}
        with self.assertRaisesRegex(SystemExit, "not testonly=True"):
            checker.check_bazel_rules(root, inventory, packages)

    def test_live_test_only_feature_proof_must_enable_exact_feature(self) -> None:
        temporary, root, packages = self.fixture(
            'rust_library(name = "lib", srcs = ["lib.rs"])\n'
            'rust_library(name = "support", testonly = True, srcs = ["lib.rs"])\n'
        )
        self.addCleanup(temporary.cleanup)
        inventory = minimal_inventory(bazel_only=["//crate:support"])
        entry = inventory["packages"]["crate"]
        entry["test_only_features"] = ["support"]
        entry["test_only_feature_targets"] = {"support": ["//crate:support"]}
        labels, builds = self.live_files(
            root,
            ["//crate:lib", "//crate:support"],
            {
                "//crate:lib": 'rust_library(name = "lib", srcs = ["//crate:lib.rs"])\n',
                "//crate:support": (
                    'rust_library(name = "support", testonly = True, '
                    'srcs = ["//crate:lib.rs"], rustc_flags = [])\n'
                ),
            },
        )
        with self.assertRaisesRegex(SystemExit, "test-only feature proof mismatch"):
            checker.check_live_bazel(root, inventory, packages, labels, builds)

    def test_configured_root_mismatch_fails(self) -> None:
        temporary, root, packages = self.fixture(
            'rust_library(name = "lib", srcs = ["lib.rs"])\n'
        )
        self.addCleanup(temporary.cleanup)
        inventory = json.loads(json.dumps(minimal_inventory()))
        inventory["bazel_roots"] = ["//crate:other"]
        with self.assertRaisesRegex(SystemExit, "configured Bazel roots mismatch"):
            checker.check_bazel_rules(root, inventory, packages)

    def test_label_ownership_is_globally_unique(self) -> None:
        inventory = minimal_inventory()
        inventory["packages"]["other"] = json.loads(json.dumps(inventory["packages"]["crate"]))
        with self.assertRaisesRegex(SystemExit, "owned by both"):
            checker.declared_labels(inventory)


if __name__ == "__main__":
    unittest.main()
