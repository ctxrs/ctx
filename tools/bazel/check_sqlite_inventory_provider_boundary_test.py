#!/usr/bin/env python3
"""Adversarial mutations for the SQLite inventory provider boundary."""

from __future__ import annotations

import contextlib
import io
import tempfile
import unittest
from pathlib import Path

from check_sqlite_inventory_provider_boundary import (
    BoundaryError,
    EXPECTED_HERMES_INTERNAL,
    EXPECTED_INTERNAL,
    main,
    validate_build,
    validate_capture_absence,
    validate_composition_ownership,
    validate_hermes_build,
    validate_hermes_manifest,
    validate_hermes_sources,
    validate_manifest,
    validate_pack_sources,
)


def facade_text() -> str:
    return (
        "use ctx_history_providers_sqlite_inventory::registration::{\n"
        "    astrbot_registration_scoped, astrbot_released_registration_scoped,\n"
        "    crush_registration_scoped, lingma_registration_scoped, shelley_registration,\n"
        "    SqliteInventoryCoverage,\n"
        "};\n"
        "pub type SqliteInventoryRouteAuthority = "
        "(Option<[u8; 32]>, SqliteInventoryCoverage);\n"
        "pub fn register_astrbot_source_backed_route() {\n"
        "    install_sqlite_inventory_registration(astrbot_registration_scoped::<L, S>());\n"
        "}\n"
        "pub fn register_astrbot_released_source_backed_route() {\n"
        "    install_sqlite_inventory_registration("
        "astrbot_released_registration_scoped::<L, S>());\n"
        "}\n"
        "pub fn register_crush_source_backed_route() {\n"
        "    install_sqlite_inventory_registration(crush_registration_scoped::<I, L, S>());\n"
        "}\n"
        "pub fn register_lingma_source_backed_route(\n"
        "    route_authority: SqliteInventoryRouteAuthority,\n"
        ") {\n"
        "    install_sqlite_inventory_registration(lingma_registration_scoped::<L, S>());\n"
        "}\n"
        "pub fn register_shelley_source_backed_route() {\n"
        "    install_sqlite_inventory_registration(shelley_registration::<L, S>());\n"
        "}\n"
    )


def hermes_facade_text() -> str:
    return (
        "use ctx_history_provider_hermes::registration::{\n"
        "    hermes_automatic_registration_scoped, hermes_explicit_registration,\n"
        "    hermes_explicit_registration_scoped, hermes_released_registration_scoped,\n"
        "};\n"
        "pub(super) fn register_hermes_source_backed_route() {\n"
        "    install_hermes_registration(hermes_automatic_registration_scoped::<L, S>());\n"
        "    install_hermes_registration(hermes_explicit_registration_scoped::<L, S>());\n"
        "}\n"
        "pub(in crate::source_backed) fn register_hermes_released_source_backed_route() {\n"
        "    install_hermes_registration(hermes_released_registration_scoped::<L, S>());\n"
        "}\n"
        "pub fn register_hermes_explicit_source_backed_route() {\n"
        "    install_hermes_registration(hermes_explicit_registration::<L, S>());\n"
        "}\n"
    )


class SqliteInventoryProviderBoundaryMutations(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)

        self.pack = self.root / "pack"
        self.pack_root = self.pack / "src"
        providers = self.pack_root / "provider/providers"
        providers.mkdir(parents=True)
        (providers / "mod.rs").write_text(
            "pub mod astrbot;\npub mod crush;\npub mod lingma;\npub mod shelley;\n",
            encoding="utf-8",
        )
        (self.pack_root / "registration.rs").write_text(
            "pub fn astrbot_registration() {}\npub fn crush_registration() {}\n"
            "pub fn discovered_lingma_registration() {}\npub fn lingma_registration() {}\n"
            "pub fn shelley_registration() {}\n",
            encoding="utf-8",
        )
        self.pack_lib = self.pack_root / "lib.rs"
        self.pack_lib.write_text("", encoding="utf-8")
        self.manifest = self.pack / "Cargo.toml"
        dependencies = "".join(
            f'{dependency} = "1"\n' for dependency in sorted(EXPECTED_INTERNAL)
        )
        self.manifest.write_text(
            "[features]\ntest-support = []\n\n[dependencies]\n"
            f"{dependencies}\n[dev-dependencies]\n",
            encoding="utf-8",
        )
        self.pack_build = self.pack / "BUILD.bazel"
        self.pack_build.write_text(
            "deps = [\n"
            + "".join(
                f'    "//crates/{dependency}:lib",\n'
                for dependency in sorted(EXPECTED_INTERNAL)
            )
            + "]\n",
            encoding="utf-8",
        )

        self.hermes = self.root / "hermes"
        self.hermes_root = self.hermes / "src"
        self.hermes_root.mkdir(parents=True)
        self.hermes_lib = self.hermes_root / "lib.rs"
        self.hermes_lib.write_text(
            "mod provider;\npub mod registration;\n", encoding="utf-8"
        )
        (self.hermes_root / "provider.rs").write_text(
            "pub fn provider_root() {}\n", encoding="utf-8"
        )
        (self.hermes_root / "registration.rs").write_text(
            "pub fn hermes_automatic_registration() {}\n"
            "pub fn hermes_explicit_registration() {}\n",
            encoding="utf-8",
        )
        self.hermes_manifest = self.hermes / "Cargo.toml"
        hermes_dependencies = "".join(
            f'{dependency} = "1"\n'
            for dependency in sorted(EXPECTED_HERMES_INTERNAL)
        )
        self.hermes_manifest.write_text(
            "[features]\ntest-support = []\n\n[dependencies]\n"
            f"{hermes_dependencies}\n[dev-dependencies]\n",
            encoding="utf-8",
        )
        self.hermes_build = self.hermes / "BUILD.bazel"
        self.hermes_build.write_text(
            "deps = [\n"
            + "".join(
                f'    "//crates/{dependency}:lib",\n'
                for dependency in sorted(EXPECTED_HERMES_INTERNAL)
            )
            + "]\n",
            encoding="utf-8",
        )

        self.composition_root = self.root / "composition/src"
        self.composition_lib = self.composition_root / "lib.rs"
        self.composition_lib.parent.mkdir(parents=True)
        self.composition_lib.write_text("", encoding="utf-8")
        self.composition_facade = (
            self.composition_root
            / "source_backed/registration/families/sqlite_inventory.rs"
        )
        self.composition_facade.parent.mkdir(parents=True)
        self.composition_facade.write_text(facade_text(), encoding="utf-8")
        self.hermes_composition_facade = (
            self.composition_root / "source_backed/registration/families/hermes.rs"
        )
        self.hermes_composition_facade.write_text(
            hermes_facade_text(), encoding="utf-8"
        )

        self.capture_root = self.root / "capture/src"
        capture_providers = self.capture_root / "provider/providers"
        capture_providers.mkdir(parents=True)
        (capture_providers / "mod.rs").write_text("", encoding="utf-8")
        self.capture_lib = self.capture_root / "lib.rs"
        self.capture_lib.write_text("", encoding="utf-8")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def validate(self) -> None:
        validate_manifest(self.manifest)
        validate_build(self.pack_build)
        validate_pack_sources(self.pack_root)
        validate_hermes_manifest(self.hermes_manifest)
        validate_hermes_build(self.hermes_build)
        validate_hermes_sources(self.hermes_root)
        validate_composition_ownership(self.composition_root)
        validate_capture_absence(self.capture_root)

    def append_manifest(self, contents: str) -> None:
        with self.manifest.open("a", encoding="utf-8") as manifest:
            manifest.write(contents)

    def assert_main_fails_closed(self) -> None:
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(
                main(
                    [
                        str(self.manifest),
                        str(self.pack_build),
                        str(self.pack_lib),
                        str(self.hermes_manifest),
                        str(self.hermes_build),
                        str(self.hermes_lib),
                        str(self.composition_lib),
                        str(self.capture_lib),
                    ]
                ),
                1,
            )

    def test_exact_composition_owned_boundary_passes(self) -> None:
        self.validate()

    def test_extra_pack_registration_owner_is_rejected(self) -> None:
        registration = self.pack_root / "registration.rs"
        registration.write_text(
            registration.read_text(encoding="utf-8")
            + "pub(crate) fn duplicate_registration() {}\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "registration authority drifted"):
            self.validate()

    def test_digit_bearing_extra_pack_registration_owner_is_rejected(self) -> None:
        registration = self.pack_root / "registration.rs"
        registration.write_text(
            registration.read_text(encoding="utf-8")
            + "pub(crate) fn duplicate2_registration() {}\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(
            BoundaryError, r"extra=\['duplicate2_registration'\]"
        ):
            self.validate()

    def test_digit_bearing_extra_provider_module_is_rejected(self) -> None:
        providers = self.pack_root / "provider/providers/mod.rs"
        providers.write_text(
            providers.read_text(encoding="utf-8") + "pub(crate) mod extra2;\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, r"actual=.*extra2"):
            self.validate()

    def test_duplicate_expected_pack_registration_owner_is_rejected(self) -> None:
        (self.pack_root / "duplicate_owner.rs").write_text(
            "pub fn astrbot_registration() {}\n", encoding="utf-8"
        )
        with self.assertRaisesRegex(
            BoundaryError, r"duplicates=\['astrbot_registration'\]"
        ):
            self.validate()

    def test_composition_facade_growth_is_rejected(self) -> None:
        self.composition_facade.write_text(
            self.composition_facade.read_text(encoding="utf-8")
            + "pub fn register_duplicate_source_backed_route() {}\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "façade item surface drifted"):
            self.validate()

    def test_missing_composition_facade_function_is_rejected(self) -> None:
        self.composition_facade.write_text(
            self.composition_facade.read_text(encoding="utf-8").replace(
                "pub fn register_astrbot_released_source_backed_route() {\n"
                "    install_sqlite_inventory_registration("
                "astrbot_released_registration_scoped::<L, S>());\n"
                "}\n",
                "",
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "façade item surface drifted"):
            self.validate()

    def test_digit_bearing_extra_composition_binding_is_rejected(self) -> None:
        self.composition_facade.write_text(
            self.composition_facade.read_text(encoding="utf-8").replace(
                "    crush_registration_scoped, lingma_registration_scoped, "
                "shelley_registration,\n",
                "    crush_registration_scoped, duplicate2_registration_scoped, "
                "lingma_registration_scoped, shelley_registration,\n",
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(
            BoundaryError, r"provider bindings drifted:.*duplicate2_registration_scoped"
        ):
            self.validate()

    def test_missing_composition_provider_binding_is_rejected(self) -> None:
        self.composition_facade.write_text(
            self.composition_facade.read_text(encoding="utf-8").replace(
                "    astrbot_registration_scoped, ", "    ", 1
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "provider bindings drifted"):
            self.validate()

    def test_missing_composition_coverage_binding_is_rejected(self) -> None:
        self.composition_facade.write_text(
            self.composition_facade.read_text(encoding="utf-8").replace(
                "    SqliteInventoryCoverage,\n", ""
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "provider bindings drifted"):
            self.validate()

    def test_digit_bearing_extra_composition_call_is_rejected(self) -> None:
        self.composition_facade.write_text(
            self.composition_facade.read_text(encoding="utf-8").replace(
                "    install_sqlite_inventory_registration("
                "astrbot_registration_scoped::<L, S>());\n",
                "    install_sqlite_inventory_registration("
                "astrbot_registration_scoped::<L, S>());\n"
                "    duplicate2_registration_scoped::<L, S>();\n",
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(
            BoundaryError,
            r"constructor calls drifted: unexpected=\['duplicate2_registration_scoped'\]",
        ):
            self.validate()

    def test_route_authority_alias_shape_drift_is_rejected(self) -> None:
        self.composition_facade.write_text(
            self.composition_facade.read_text(encoding="utf-8").replace(
                "(Option<[u8; 32]>, SqliteInventoryCoverage);",
                "([u8; 32], SqliteInventoryCoverage);",
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "route-authority alias drifted"):
            self.validate()

    def test_restricted_composition_public_surface_is_rejected(self) -> None:
        self.composition_facade.write_text(
            self.composition_facade.read_text(encoding="utf-8").replace(
                "pub fn register_astrbot_released_source_backed_route()",
                "pub(crate) fn register_astrbot_released_source_backed_route()",
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "public surface drifted"):
            self.validate()

    def test_missing_composition_facade_is_rejected(self) -> None:
        self.composition_facade.unlink()
        with self.assertRaisesRegex(
            BoundaryError, "composition SQLite inventory façade is missing"
        ):
            self.validate()

    def test_missing_hermes_composition_facade_is_rejected(self) -> None:
        self.hermes_composition_facade.unlink()
        with self.assertRaisesRegex(BoundaryError, "composition Hermes façade is missing"):
            self.validate()

    def test_hermes_extra_registration_owner_is_rejected(self) -> None:
        registration = self.hermes_root / "registration.rs"
        registration.write_text(
            registration.read_text(encoding="utf-8")
            + "pub fn duplicate_registration() {}\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "Hermes registration authority drifted"):
            self.validate()

    def test_extra_hermes_composition_binding_is_rejected(self) -> None:
        self.hermes_composition_facade.write_text(
            self.hermes_composition_facade.read_text(encoding="utf-8").replace(
                "    hermes_explicit_registration_scoped, "
                "hermes_released_registration_scoped,\n",
                "    duplicate_registration_scoped, hermes_explicit_registration_scoped, "
                "hermes_released_registration_scoped,\n",
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "provider bindings drifted"):
            self.validate()

    def test_missing_hermes_composition_binding_is_rejected(self) -> None:
        self.hermes_composition_facade.write_text(
            self.hermes_composition_facade.read_text(encoding="utf-8").replace(
                "    hermes_explicit_registration_scoped, ", "    ", 1
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "provider bindings drifted"):
            self.validate()

    def test_hermes_cannot_depend_on_finite_inventory_pack(self) -> None:
        self.hermes_manifest.write_text(
            self.hermes_manifest.read_text(encoding="utf-8").replace(
                "\n[dev-dependencies]\n",
                '\nctx-history-providers-sqlite-inventory = "1"\n'
                "\n[dev-dependencies]\n",
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "dependency inventory drifted"):
            self.validate()

    def test_missing_composition_registration_is_rejected(self) -> None:
        self.composition_facade.write_text(
            self.composition_facade.read_text(encoding="utf-8").replace(
                "    install_sqlite_inventory_registration("
                "crush_registration_scoped::<I, L, S>());\n",
                "",
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "constructor calls drifted"):
            self.validate()

    def test_capture_facade_reintroduction_is_rejected(self) -> None:
        facade = (
            self.capture_root
            / "provider/source_backed/registration/families/sqlite_inventory.rs"
        )
        facade.parent.mkdir(parents=True)
        facade.write_text(facade_text(), encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "capture reacquired"):
            self.validate()

    def test_capture_duplicate_authority_is_rejected(self) -> None:
        owner = (
            self.capture_root
            / "provider/source_backed/registration/families/sqlite/inventory.rs"
        )
        owner.parent.mkdir(parents=True)
        owner.write_text("pub fn astrbot_registration() {}\n", encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "capture reacquired"):
            self.validate()

    def test_capture_provider_reintroduction_is_rejected(self) -> None:
        providers = self.capture_root / "provider/providers/mod.rs"
        providers.write_text("pub(crate) mod astrbot;\n", encoding="utf-8")
        with self.assertRaisesRegex(
            BoundaryError, "capture retains production ownership"
        ):
            self.validate()

    def test_inline_cfg_test_open_options_fixture_is_excluded(self) -> None:
        (self.pack_root / "provider.rs").write_text(
            "pub fn production_reader() {}\n"
            "#[cfg(test)]\nmod fixtures {\n"
            "    use std::fs::OpenOptions;\n"
            '    const UNBALANCED_BRACE: &str = "{";\n'
            "    const CLOSING_BRACE: char = '}';\n"
            "    fn rewrite_fixture() { let _ = OpenOptions::new(); }\n"
            "}\n",
            encoding="utf-8",
        )
        self.validate()

    def test_production_open_options_is_rejected_even_with_inline_test_fixture(self) -> None:
        (self.pack_root / "provider.rs").write_text(
            "use std::fs::OpenOptions;\n"
            "pub fn production_writer() { let _ = OpenOptions::new(); }\n"
            "#[cfg(test)]\nmod fixtures {\n"
            "    use std::fs::OpenOptions;\n"
            "    fn rewrite_fixture() { let _ = OpenOptions::new(); }\n"
            "}\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "write-capable API"):
            self.validate()

    def test_missing_pack_dependency_is_rejected(self) -> None:
        self.manifest.write_text(
            self.manifest.read_text(encoding="utf-8").replace(
                'ctx-history-provider-runtime = "1"\n', ""
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "dependency inventory drifted"):
            self.validate()

    def test_aliased_build_dependency_is_rejected(self) -> None:
        self.append_manifest(
            "\n[build-dependencies]\n"
            'hidden_capture = { package = "ctx-history-capture", version = "1" }\n'
        )
        with self.assertRaisesRegex(BoundaryError, "ctx-history-capture"):
            self.validate()

    def test_aliased_target_dependency_is_rejected(self) -> None:
        self.append_manifest(
            "\n[target.'cfg(unix)'.dependencies]\n"
            'hidden_index = { package = "ctx-history-index", version = "1" }\n'
        )
        with self.assertRaisesRegex(BoundaryError, "ctx-history-index"):
            self.validate()

    def test_aliased_target_dev_dependency_is_rejected(self) -> None:
        self.append_manifest(
            "\n[target.'cfg(test)'.dev-dependencies]\n"
            'hidden_capture = { package = "ctx-history-capture", version = "1" }\n'
        )
        with self.assertRaisesRegex(BoundaryError, "ctx-history-capture"):
            self.validate()

    def test_aliased_target_build_dependency_is_rejected(self) -> None:
        self.append_manifest(
            "\n[target.'cfg(windows)'.build-dependencies]\n"
            'hidden_index = { package = "ctx-history-index", version = "1" }\n'
        )
        with self.assertRaisesRegex(BoundaryError, "ctx-history-index"):
            self.validate()

    def test_malformed_manifest_fails_closed(self) -> None:
        self.manifest.write_text("[dependencies\n", encoding="utf-8")
        self.assert_main_fails_closed()

    def test_unreadable_input_fails_closed(self) -> None:
        self.pack_build.unlink()
        self.pack_build.mkdir()
        self.assert_main_fails_closed()


if __name__ == "__main__":
    unittest.main()
