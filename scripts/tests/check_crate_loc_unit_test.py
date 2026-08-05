#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


CHECKER = Path(__file__).resolve().parents[1] / "check-crate-loc.py"
sys.path.insert(0, str(CHECKER.parent))
SPEC = importlib.util.spec_from_file_location("crate_gate", CHECKER)
assert SPEC is not None and SPEC.loader is not None
gate = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = gate
SPEC.loader.exec_module(gate)


PLATFORMS = [
    gate.Platform("freebsd-x64", "freebsd", "x86_64", "", "x86_64-unknown-freebsd", "//p:freebsd"),
    gate.Platform("linux-arm64", "linux", "aarch64", "gnu", "aarch64-unknown-linux-gnu", "//p:linux_arm64"),
    gate.Platform("linux-x64", "linux", "x86_64", "gnu", "x86_64-unknown-linux-gnu", "//p:linux_x64"),
    gate.Platform("macos-arm64", "macos", "aarch64", "", "aarch64-apple-darwin", "//p:macos_arm64"),
    gate.Platform("macos-x64", "macos", "x86_64", "", "x86_64-apple-darwin", "//p:macos_x64"),
    gate.Platform("windows-x64", "windows", "x86_64", "gnu", "x86_64-pc-windows-gnu", "//p:windows"),
]


class Fixture:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)

    def write(self, path: str, value: str) -> None:
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(value, encoding="utf-8")

    def view(self) -> gate.SourceView:
        paths = {path.relative_to(self.root).as_posix() for path in self.root.rglob("*") if path.is_file()}
        return gate.SourceView(self.root, paths)

    def close(self) -> None:
        self.temporary.cleanup()


class CrateGateUnitTest(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()
        self.addCleanup(self.fixture.close)

    def walk(
        self,
        root: str,
        package_root: str = "crate",
        out_dir_sources: dict[str, str | None] | None = None,
    ) -> tuple[set[str], set[str]]:
        target = gate.CargoTarget("lib:fixture", "lib", "fixture", root)
        return gate.target_source_inventory(
            self.fixture.view(), package_root, target, PLATFORMS[2], [], out_dir_sources
        )

    def test_outside_src_custom_target_and_orphan_exclusion(self) -> None:
        self.fixture.write("crate/Cargo.toml", '[package]\nname="fixture"\nversion="0.1.0"\n[lib]\npath="engine/root.rs"\n')
        self.fixture.write("crate/engine/root.rs", 'mod reached;\n#[path = "../shared.rs"] mod shared;\n')
        self.fixture.write("crate/engine/reached.rs", "pub fn reached() {}\n")
        self.fixture.write("crate/shared.rs", "pub fn shared() {}\n")
        self.fixture.write("crate/src/orphan.rs", "pub fn orphan() {}\n")
        package = {"package": "fixture", "root": "crate", "cargo": gate.load_toml_text(self.fixture.view().read_text("crate/Cargo.toml"), "Cargo.toml")}
        target = gate.cargo_targets(self.fixture.view(), package)["lib:fixture"]
        sources = gate.target_sources(self.fixture.view(), "crate", target, PLATFORMS[2], [])
        self.assertEqual(sources, {"crate/engine/root.rs", "crate/engine/reached.rs", "crate/shared.rs"})

    def test_lib_bin_union_deduplicates_shared_modules(self) -> None:
        self.fixture.write("crate/src/lib.rs", '#[path = "shared.rs"] mod shared;\n')
        self.fixture.write("crate/src/main.rs", '#[path = "shared.rs"] mod shared;\nfn main() {}\n')
        self.fixture.write("crate/src/shared.rs", "pub fn shared() {}\n")
        view = self.fixture.view()
        lib = gate.target_sources(view, "crate", gate.CargoTarget("lib:x", "lib", "x", "crate/src/lib.rs"), PLATFORMS[2], [])
        binary = gate.target_sources(view, "crate", gate.CargoTarget("bin:x", "bin", "x", "crate/src/main.rs"), PLATFORMS[2], [])
        self.assertEqual(len(lib | binary), 3)

    def test_build_helper_include_path_test_name_and_generated_output(self) -> None:
        self.fixture.write(
            "crate/build.rs",
            'mod helper;\n#[path = "generated/checked.rs"] mod checked;\n'
            'include!{concat!(env!("CARGO_MANIFEST_DIR"), "/test_named.rs")}\n'
            'include![concat!(env!("OUT_DIR"), "/copied.rs")];\n'
            'include!(concat!(env!("OUT_DIR"), "/bazel_generated.rs"));\nfn main() {}\n',
        )
        self.fixture.write("crate/helper.rs", "pub fn helper() {}\n")
        self.fixture.write("crate/generated/checked.rs", "pub fn checked_in_generated() {}\n")
        self.fixture.write("crate/generated/copied.rs", "pub fn copied_through_out_dir() {}\n")
        self.fixture.write("crate/test_named.rs", "pub fn production_even_when_test_named() {}\n")
        active, loaded = self.walk(
            "crate/build.rs",
            out_dir_sources={"bazel_generated.rs": None, "copied.rs": "generated/copied.rs"},
        )
        expected = {
            "crate/build.rs",
            "crate/helper.rs",
            "crate/generated/checked.rs",
            "crate/generated/copied.rs",
            "crate/test_named.rs",
        }
        self.assertEqual(active, expected)
        self.assertEqual(loaded, expected)

    def test_out_dir_include_without_exact_provenance_fails_closed(self) -> None:
        self.fixture.write(
            "crate/build.rs",
            'include!(concat!(env!("OUT_DIR"), "/unknown.rs"));\nfn main() {}\n',
        )
        with self.assertRaisesRegex(gate.GateError, "lacks exact generated/checked-in provenance"):
            self.walk("crate/build.rs")

    def test_cfg_platform_union_and_cfg_test_only_file(self) -> None:
        self.fixture.write(
            "crate/src/lib.rs",
            '#[cfg(target_os = "windows")] mod windows;\n'
            '#[cfg(target_os = "freebsd")] mod freebsd;\n'
            '#[cfg_attr(target_os = "windows", path = "windows_special.rs")] mod special;\n'
            '#[cfg(test)] mod hidden;\n'
            'include!("tests.rs");\npub fn common() {}\n',
        )
        self.fixture.write("crate/src/windows.rs", "pub fn platform() {}\n")
        self.fixture.write("crate/src/freebsd.rs", "pub fn platform() {}\n")
        self.fixture.write("crate/src/special.rs", "pub fn ordinary() {}\n")
        self.fixture.write("crate/src/windows_special.rs", "pub fn windows() {}\n")
        self.fixture.write("crate/src/hidden.rs", "pub fn test_only() {}\n")
        self.fixture.write("crate/src/tests.rs", "#[cfg(test)] mod tests { fn only_test() {} }\n")
        view = self.fixture.view()
        target = gate.CargoTarget("lib:x", "lib", "x", "crate/src/lib.rs")
        active: set[str] = set()
        loaded: set[str] = set()
        for platform in PLATFORMS:
            one_active, one_loaded = gate.target_source_inventory(view, "crate", target, platform, [])
            active |= one_active
            loaded |= one_loaded
        self.assertIn("crate/src/windows.rs", active)
        self.assertIn("crate/src/freebsd.rs", active)
        self.assertIn("crate/src/special.rs", active)
        self.assertIn("crate/src/windows_special.rs", active)
        self.assertNotIn("crate/src/hidden.rs", loaded)
        self.assertIn("crate/src/tests.rs", loaded)
        self.assertNotIn("crate/src/tests.rs", active)

    def test_workspace_root_automatic_member_and_dev_edge_exclusion(self) -> None:
        self.fixture.write(
            "Cargo.toml",
            '[package]\nname="root"\nversion="0.1.0"\n'
            '[workspace]\nmembers=[]\n[workspace.dependencies]\na={path="a"}\n'
            '[dependencies]\na={workspace=true}\n[dev-dependencies]\nb={path="b"}\n',
        )
        self.fixture.write("src/lib.rs", "pub fn root() {}\n")
        self.fixture.write("a/Cargo.toml", '[package]\nname="a"\nversion="0.1.0"\n[dependencies]\nb={path="../b"}\n')
        self.fixture.write("a/src/lib.rs", "pub fn a() {}\n")
        self.fixture.write("b/Cargo.toml", '[package]\nname="b"\nversion="0.1.0"\n')
        self.fixture.write("b/src/lib.rs", "pub fn b() {}\n")
        packages = gate.workspace_packages(self.fixture.view())
        self.assertEqual([item["package"] for item in packages], ["a", "b", "root"])
        edges = gate.workspace_edges(self.fixture.view(), packages, PLATFORMS)
        self.assertEqual(edges["linux-x64"], {("a", "b"), ("root", "a")})

    def test_in_workspace_path_dependency_missing_from_runfiles_fails_closed(self) -> None:
        self.fixture.write("Cargo.toml", '[workspace]\nmembers=["crate"]\n')
        self.fixture.write(
            "crate/Cargo.toml",
            '[package]\nname="fixture"\nversion="0.1.0"\n[dependencies]\nghost={path="../ghost"}\n',
        )
        self.fixture.write("crate/src/lib.rs", "pub fn production() {}\n")
        with self.assertRaisesRegex(gate.GateError, "in-workspace path dependency manifest is absent"):
            gate.workspace_packages(self.fixture.view())

    def test_examples_benches_and_tests_are_not_production_targets(self) -> None:
        self.fixture.write("crate/Cargo.toml", '[package]\nname="fixture"\nversion="0.1.0"\n')
        for path in ("src/lib.rs", "tests/check.rs", "examples/demo.rs", "benches/speed.rs"):
            self.fixture.write(f"crate/{path}", "pub fn item() {}\n")
        view = self.fixture.view()
        cargo = gate.load_toml_text(view.read_text("crate/Cargo.toml"), "Cargo.toml")
        package = {"package": "fixture", "root": "crate", "cargo": cargo}
        targets = gate.cargo_targets(view, package)
        production = {key for key, target in targets.items() if target.kind in {"lib", "bin", "custom-build"}}
        self.assertEqual(production, {"lib:fixture"})

    def test_inventory_explicitly_excludes_test_only_binary(self) -> None:
        self.fixture.write("Cargo.toml", '[workspace]\nmembers=["crate"]\n')
        self.fixture.write(
            "crate/Cargo.toml",
            '[package]\nname="fixture"\nversion="0.1.0"\n'
            '[[bin]]\nname="fixture-test"\npath="tools/test.rs"\nrequired-features=["test-only"]\n'
            '[features]\ntest-only=[]\n',
        )
        self.fixture.write("crate/src/lib.rs", "pub fn production() {}\n")
        self.fixture.write("crate/tools/test.rs", "fn main() {}\n")
        inventory = {
            "packages": {
                "fixture": {
                    "manifest": "crate/Cargo.toml",
                    "targets": {"bin:fixture-test": "//crate:test_bin", "lib:fixture": "//crate:lib"},
                    "production_targets": {"lib:fixture": [{"features": [], "kind": "rust", "label": "//crate:lib"}]},
                    "test_only_targets": ["bin:fixture-test"],
                    "production_features": [],
                    "test_only_features": ["test-only"],
                    "test_only_feature_targets": {"test-only": ["//crate:test_bin"]},
                    "out_dir_sources": {},
                    "native_unit": None,
                    "focused_tests": [],
                    "bazel_only_targets": [],
                }
            }
        }
        packages = gate.workspace_packages(self.fixture.view())
        validated = gate.validate_inventory_packages(self.fixture.view(), packages, inventory)
        self.assertEqual(set(validated["fixture"]["entry"]["production_targets"]), {"lib:fixture"})

    def test_test_only_binary_without_required_feature_cannot_be_excluded(self) -> None:
        self.fixture.write("Cargo.toml", '[workspace]\nmembers=["crate"]\n')
        self.fixture.write(
            "crate/Cargo.toml",
            '[package]\nname="fixture"\nversion="0.1.0"\n[[bin]]\nname="fixture-test"\npath="tools/test.rs"\n',
        )
        self.fixture.write("crate/src/lib.rs", "pub fn production() {}\n")
        self.fixture.write("crate/tools/test.rs", "fn main() {}\n")
        inventory = {
            "packages": {
                "fixture": {
                    "manifest": "crate/Cargo.toml",
                    "targets": {"bin:fixture-test": "//crate:test_bin", "lib:fixture": "//crate:lib"},
                    "production_targets": {"lib:fixture": [{"features": [], "kind": "rust", "label": "//crate:lib"}]},
                    "test_only_targets": ["bin:fixture-test"],
                    "production_features": [],
                    "test_only_features": [],
                    "test_only_feature_targets": {},
                    "out_dir_sources": {},
                    "native_unit": None,
                    "focused_tests": [],
                    "bazel_only_targets": [],
                }
            }
        }
        with self.assertRaisesRegex(gate.GateError, "test_only_targets is invalid"):
            gate.validate_inventory_packages(self.fixture.view(), gate.workspace_packages(self.fixture.view()), inventory)

    def test_production_feature_combinations_are_complete_and_deterministic(self) -> None:
        self.assertEqual(gate.feature_combinations(["a", "b"]), [[], ["a"], ["b"], ["a", "b"]])

    def test_feature_cfg_union_includes_enabled_and_disabled_variants(self) -> None:
        self.fixture.write(
            "crate/src/lib.rs",
            '#[cfg(feature = "extra")] mod enabled;\n'
            '#[cfg(not(feature = "extra"))] mod disabled;\n',
        )
        self.fixture.write("crate/src/enabled.rs", "pub fn enabled() {}\n")
        self.fixture.write("crate/src/disabled.rs", "pub fn disabled() {}\n")
        view = self.fixture.view()
        target = gate.CargoTarget("lib:x", "lib", "x", "crate/src/lib.rs")
        union: set[str] = set()
        for features in gate.feature_combinations(["extra"]):
            union.update(gate.target_sources(view, "crate", target, PLATFORMS[2], features))
        self.assertEqual(
            union,
            {"crate/src/lib.rs", "crate/src/enabled.rs", "crate/src/disabled.rs"},
        )

    def test_self_loop_and_three_node_cycles_are_deterministic(self) -> None:
        edges = {
            "linux-x64": {("a", "a"), ("a", "b"), ("b", "c"), ("c", "a")},
            "windows-x64": {("c", "a"), ("a", "b"), ("b", "c")},
        }
        self.assertEqual(
            gate.graph_cycles(edges),
            [
                {"cycle": ["a", "a"], "platforms": ["linux-x64"]},
                {"cycle": ["a", "b", "c", "a"], "platforms": ["linux-x64", "windows-x64"]},
            ],
        )

    def test_forbidden_stale_and_cargo_bazel_parity_mismatches(self) -> None:
        cargo = {"linux-x64": {("a", "b"), ("b", "c")}}
        expected = {"linux-x64": {("a", "b"), ("c", "d")}}
        bazel = {"linux-x64": {("a", "b"), ("d", "a")}}
        codes = [item["code"] for item in gate.graph_edge_violations(cargo, expected, bazel)]
        self.assertEqual(codes, ["forbidden_edge", "stale_edge", "extra_bazel_edge", "missing_bazel_edge"])

    def test_configured_action_source_mismatch(self) -> None:
        violations = gate.source_action_violations(
            {"crate/root.rs", "crate/helper.rs"},
            {"crate/root.rs", "crate/orphan.rs"},
            platform="linux-x64",
            label="//crate:lib",
        )
        self.assertEqual(violations[0]["code"], "missing_bazel_sources")
        self.assertEqual(violations[0]["sources"], ["crate/helper.rs"])

    def test_canonical_json_is_order_independent(self) -> None:
        left = {"status": "pass", "violations": [], "schema_version": 2}
        right = {"schema_version": 2, "violations": [], "status": "pass"}
        self.assertEqual(gate.canonical_bytes(left), gate.canonical_bytes(right))

    def test_exception_shrink_and_removal_are_valid_policy_states(self) -> None:
        snapshot = {
            "exceptions": [{"exception_id": "legacy", "package": "large", "maximum_cloc": 25000}],
            "package_by_name": {"large": {"production_cloc": 25000}},
        }
        policy = {"hard_limit": 20000, "grandfathered": [{"exception_id": "legacy", "package": "large", "code_baseline": 24000}]}
        self.assertEqual(gate.validate_ledger(policy, snapshot)["large"]["code_baseline"], 24000)
        policy["grandfathered"] = []
        self.assertEqual(gate.validate_ledger(policy, snapshot), {})
        policy["grandfathered"] = [{"exception_id": "legacy", "package": "large", "code_baseline": 25001}]
        with self.assertRaisesRegex(gate.GateError, "invalid no-growth ceiling"):
            gate.validate_ledger(policy, snapshot)

    def test_temporary_edge_identity_can_only_be_removed(self) -> None:
        edge = {"exception_id": "temporary", "from": "a", "to": "b"}
        snapshot = {"temporary_edges": [edge], "workspace_edges": [{"from": "a", "to": "b"}]}
        policy = {"temporary_edges": [{**edge, "introduced_at": gate.SNAPSHOT}]}
        self.assertEqual(gate.validate_temporary_edges(policy, snapshot), {"temporary": ("a", "b")})
        self.assertEqual(gate.validate_temporary_edges({"temporary_edges": []}, snapshot), {})
        policy["temporary_edges"][0]["to"] = "c"
        with self.assertRaisesRegex(gate.GateError, "unknown or reassigned"):
            gate.validate_temporary_edges(policy, snapshot)

    def test_snapshot_inventory_hash_is_immutable(self) -> None:
        snapshot = CHECKER.parent / "check-crate-loc-snapshot-v1.json"
        target = self.fixture.root / "scripts/check-crate-loc-snapshot-v1.json"
        target.parent.mkdir(parents=True)
        target.write_bytes(snapshot.read_bytes())
        policy = {"snapshot_inventory": "scripts/check-crate-loc-snapshot-v1.json"}
        gate.load_snapshot_inventory(self.fixture.root, policy)
        target.write_bytes(target.read_bytes() + b" ")
        with self.assertRaisesRegex(gate.GateError, "identity changed"):
            gate.load_snapshot_inventory(self.fixture.root, policy)

    def test_policy_package_rename_add_delete_fail_closed(self) -> None:
        policy = {"packages": [{"package": "old", "manifest": "old/Cargo.toml", "production_targets": ["lib:old"], "source_digest": "0" * 64}]}
        actual = [{"package": "new", "manifest": "new/Cargo.toml", "source_digest": "1" * 64}]
        violations = gate.validate_policy_packages(policy, actual, {"new": ["lib:new"]})
        self.assertEqual([item["code"] for item in violations], ["missing_package_record", "stale_package_record"])
        moved = [{"package": "old", "manifest": "moved/Cargo.toml", "source_digest": "0" * 64}]
        violations = gate.validate_policy_packages(policy, moved, {"old": ["lib:old"]})
        self.assertEqual([item["code"] for item in violations], ["manifest_drift"])
        changed = [{"package": "old", "manifest": "old/Cargo.toml", "source_digest": "1" * 64}]
        violations = gate.validate_policy_packages(policy, changed, {"old": ["lib:old"]})
        self.assertEqual([item["code"] for item in violations], ["source_drift"])


if __name__ == "__main__":
    unittest.main()
