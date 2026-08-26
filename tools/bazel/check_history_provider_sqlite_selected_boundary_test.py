#!/usr/bin/env python3
"""Adversarial mutations for the selected SQLite provider boundary."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from check_history_provider_sqlite_selected_boundary import (
    BoundaryError,
    EXPECTED_DEV_SPECS,
    EXPECTED_FEATURES,
    EXPECTED_INTERNAL_BAZEL,
    EXPECTED_NORMAL_SPECS,
    EXPECTED_TEST_INTERNAL_BAZEL,
    PROVIDERS,
    expected_capture_facade,
    selected_sqlite_selector_authorities,
    validate_build,
    validate_capture,
    validate_manifest,
    validate_pack_tree,
)


def cargo_table(name: str, dependencies: dict[str, dict[str, object]]) -> str:
    entries = []
    for dependency, specification in sorted(dependencies.items()):
        if specification == {"workspace": True}:
            entries.append(f"{dependency}.workspace = true")
            continue
        fields = []
        for key, value in specification.items():
            if isinstance(value, list):
                rendered = "[" + ", ".join(f'\"{item}\"' for item in value) + "]"
            else:
                rendered = f'"{value}"'
            fields.append(f"{key} = {rendered}")
        entries.append(f"{dependency} = {{ {', '.join(fields)} }}")
    rendered_entries = "\n".join(entries)
    return f"[{name}]\n{rendered_entries}\n"


def bazel_assignment(name: str, dependencies: set[str]) -> str:
    entries = "\n".join(f'    "{dependency}",' for dependency in sorted(dependencies))
    return f"{name} = [\n{entries}\n]\n"


class BoundaryMutationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.pack = root / "pack"
        self.capture = root / "capture"
        self.pack_cargo = self.pack / "Cargo.toml"
        self.pack_build = self.pack / "BUILD.bazel"
        self.capture_cargo = self.capture / "Cargo.toml"
        self.capture_build = self.capture / "BUILD.bazel"

        (self.pack / "src/providers").mkdir(parents=True)
        (self.capture / "src/source_backed/family").mkdir(parents=True)
        (self.capture / "src/source_backed/registration/families/sqlite").mkdir(
            parents=True
        )
        for provider in PROVIDERS:
            provider_root = self.pack / "src/providers" / provider
            provider_root.mkdir()
            (provider_root / "mod.rs").write_text("", encoding="utf-8")

        self.pack_cargo.write_text(
            '[features]\ndefault = []\ntest-support = ["ctx-history-source-sqlite/test-support"]\n'
            + "\n"
            + cargo_table("dependencies", EXPECTED_NORMAL_SPECS)
            + "\n"
            + cargo_table("dev-dependencies", EXPECTED_DEV_SPECS),
            encoding="utf-8",
        )
        self.pack_build.write_text(
            bazel_assignment("PROVIDER_DEPS", EXPECTED_INTERNAL_BAZEL)
            + "\n"
            + bazel_assignment("PROVIDER_TEST_DEPS", EXPECTED_TEST_INTERNAL_BAZEL),
            encoding="utf-8",
        )
        pack_api = "pub trait SelectedSqliteCaptureBinding {}\n" + "".join(
            f"pub fn {provider}_source_backed_driver() {{}}\n"
            for provider in sorted(PROVIDERS)
        )
        (self.pack / "src/lib.rs").write_text(pack_api, encoding="utf-8")

        self.capture_cargo.write_text(
            "[dependencies]\n"
            'ctx-history-providers-sqlite-selected = { path = "../ctx-history-providers-sqlite-selected" }\n'
            "\n[dev-dependencies]\n"
            'ctx-history-providers-sqlite-selected = { path = "../ctx-history-providers-sqlite-selected", features = ["test-support"] }\n',
            encoding="utf-8",
        )
        self.capture_build.write_text(
            'deps = ["//crates/ctx-history-providers-sqlite-selected:lib"]\n'
            'test_deps = ["//crates/ctx-history-providers-sqlite-selected:test_support_lib"]\n',
            encoding="utf-8",
        )
        inventory = self.capture / "src/source_backed/inventory.rs"
        inventory.write_text(
            """
sqlite_route!(Firebender, "firebender", true, true, DiscoveredWinner);
sqlite_route!(Goose, "goose", true, true, SelectedWithRetainedExplicit, SelectedWithRetainedRoutes);
sqlite_route!(KiroCli, "kiro", true, true, DiscoveredWinner);
sqlite_route!(Warp, "warp", true, true, NamedSurface, NamedSurface);
""",
            encoding="utf-8",
        )
        (self.capture / "src/source_backed/family/document.rs").write_text(
            "impl ctx_history_providers_sqlite_selected::SelectedSqliteCaptureBinding for Binding {}\n",
            encoding="utf-8",
        )
        self.write_capture_composition()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def write_capture_composition(self) -> None:
        inventory = self.capture / "src/source_backed/inventory.rs"
        authorities = selected_sqlite_selector_authorities(inventory)
        facade = self.capture / "src/source_backed/registration/families/sqlite/other.rs"
        facade.write_text(expected_capture_facade(authorities), encoding="utf-8")

    def capture_facade(self) -> Path:
        return (
            self.capture
            / "src/source_backed/registration/families/sqlite/other.rs"
        )

    def validate(self) -> None:
        validate_manifest(self.pack_cargo)
        validate_build(self.pack_build)
        validate_pack_tree(self.pack)
        validate_capture(
            self.capture_cargo,
            self.capture_build,
            self.capture,
        )

    def test_exact_boundary_passes(self) -> None:
        self.validate()

    def test_cargo_dependency_drift_is_rejected(self) -> None:
        self.pack_cargo.write_text(
            self.pack_cargo.read_text()
            + '\nctx-history-capture = { path = "../ctx-history-capture" }\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "dependencies drifted"):
            self.validate()

    def test_cargo_package_rename_is_rejected(self) -> None:
        self.pack_cargo.write_text(
            self.pack_cargo.read_text().replace(
                'ctx-history-capture-model = { path = "../ctx-history-capture-model" }',
                'ctx-history-capture-model = { package = "ctx-history-capture", path = "../ctx-history-capture" }',
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "specifications drifted"):
            self.validate()

    def test_target_specific_cargo_dependency_is_rejected(self) -> None:
        self.pack_cargo.write_text(
            self.pack_cargo.read_text()
            + '\n[target.\'cfg(unix)\'.dependencies]\nctx-history-index = { path = "../ctx-history-index" }\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "target-specific dependencies"):
            self.validate()

    def test_test_support_feature_drift_is_rejected(self) -> None:
        self.pack_cargo.write_text(
            self.pack_cargo.read_text().replace(
                'test-support = ["ctx-history-source-sqlite/test-support"]',
                "test-support = []",
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "features drifted"):
            self.validate()

    def test_bazel_dependency_outside_allowlist_is_rejected(self) -> None:
        self.pack_build.write_text(
            self.pack_build.read_text()
            + '\ndata = ["//crates/ctx-history-jsonl:lib"]\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "dependency surface drifted"):
            self.validate()

    def test_bazel_upward_dependency_is_rejected(self) -> None:
        self.pack_build.write_text(
            self.pack_build.read_text()
            + '\ndeps = ["//crates/ctx-history-index:lib"]\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "dependency surface drifted"):
            self.validate()

    def test_source_upward_authority_is_rejected(self) -> None:
        (self.pack / "src/upward.rs").write_text(
            "use ctx_history_capture::CaptureError;\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "capture authority"):
            self.validate()

    def test_provider_cohort_drift_is_rejected(self) -> None:
        (self.pack / "src/providers/unexpected").mkdir()
        with self.assertRaisesRegex(BoundaryError, "cohort drifted"):
            self.validate()

    def test_missing_pack_route_authority_is_rejected(self) -> None:
        lib = self.pack / "src/lib.rs"
        lib.write_text(
            lib.read_text().replace("pub fn warp_source_backed_driver() {}\n", ""),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "does not own warp route construction"):
            self.validate()

    def test_stale_capture_provider_body_is_rejected(self) -> None:
        stale = self.capture / "src/providers/goose"
        stale.mkdir(parents=True)
        (stale / "mod.rs").write_text("", encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "retains selected SQLite provider bodies"):
            self.validate()

    def test_each_capture_selector_authority_swap_is_rejected(self) -> None:
        facade = self.capture_facade()
        original = facade.read_text()
        mutations = {
            "firebender": ("DiscoveredWinner", "NamedSurface"),
            "kiro": ("DiscoveredWinner", "NamedSurface"),
            "warp": ("NamedSurface", "DiscoveredWinner"),
            "goose": ("SelectedWithRetainedExplicit", "DiscoveredWinner"),
        }
        for provider, (authority, replacement) in mutations.items():
            with self.subTest(provider=provider):
                function = original.index(f"fn register_{provider}_source_backed_route")
                authority_start = original.index(
                    f"SourceBackedSelectorAuthority::{authority}", function
                )
                authority_end = authority_start + len(
                    f"SourceBackedSelectorAuthority::{authority}"
                )
                facade.write_text(
                    original[:authority_start]
                    + f"SourceBackedSelectorAuthority::{replacement}"
                    + original[authority_end:],
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(BoundaryError, "exact thin composition"):
                    self.validate()
        facade.write_text(original, encoding="utf-8")

    def test_inventory_authority_change_requires_matching_facade(self) -> None:
        inventory = self.capture / "src/source_backed/inventory.rs"
        inventory.write_text(
            inventory.read_text().replace(
                'sqlite_route!(Warp, "warp", true, true, NamedSurface, NamedSurface);',
                'sqlite_route!(Warp, "warp", true, true, DiscoveredWinner, NamedSurface);',
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "exact thin composition"):
            self.validate()
        self.write_capture_composition()
        self.validate()

    def test_each_scoped_pack_driver_is_required(self) -> None:
        facade = self.capture_facade()
        original = facade.read_text()
        for provider in PROVIDERS:
            with self.subTest(provider=provider):
                scoped = f"{provider}_source_backed_driver_scoped"
                function = original.index(f"fn register_{provider}_source_backed_route")
                driver = original.index(scoped, function)
                facade.write_text(
                    original[:driver]
                    + f"{provider}_source_backed_driver"
                    + original[driver + len(scoped) :],
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(BoundaryError, "exact thin composition"):
                    self.validate()
        facade.write_text(original, encoding="utf-8")

    def test_each_root_lineage_scope_is_required(self) -> None:
        facade = self.capture_facade()
        original = facade.read_text()
        scoped_lineage = """source_root_lineage.map_or(
            ctx_history_core::SourceAnchorScope::Unqualified,
            ctx_history_core::SourceAnchorScope::Lineage,
        )"""
        for provider in PROVIDERS:
            with self.subTest(provider=provider):
                function = original.index(f"fn register_{provider}_source_backed_route")
                scope = original.index(scoped_lineage, function)
                facade.write_text(
                    original[:scope]
                    + "ctx_history_core::SourceAnchorScope::Unqualified"
                    + original[scope + len(scoped_lineage) :],
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(BoundaryError, "exact thin composition"):
                    self.validate()
        facade.write_text(original, encoding="utf-8")

    def test_capture_provider_logic_growth_is_rejected(self) -> None:
        facade = self.capture_facade()
        facade.write_text(
            facade.read_text() + "\nfn capture_owned_goose_policy() { unreachable!() }\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "exact thin composition"):
            self.validate()

    def test_missing_capture_composition_is_rejected(self) -> None:
        facade = self.capture_facade()
        facade.write_text(
            facade.read_text().replace(
                "ctx_history_providers_sqlite_selected::kiro_source_backed_driver",
                "missing_kiro_source_backed_driver",
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "exact thin composition"):
            self.validate()

    def test_pack_reference_outside_binding_and_facade_is_rejected(self) -> None:
        (self.capture / "src/selected_bypass.rs").write_text(
            "use ctx_history_providers_sqlite_selected::GooseSourceRoute;\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "references escaped"):
            self.validate()


if __name__ == "__main__":
    unittest.main()
