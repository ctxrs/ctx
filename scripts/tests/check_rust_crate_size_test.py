#!/usr/bin/env python3
"""Focused tests for the physical Cargo-package CLOC gate."""

from __future__ import annotations

import ast
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "check-rust-crate-size.py"
SPEC = importlib.util.spec_from_file_location("check_rust_crate_size", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
gate = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = gate
SPEC.loader.exec_module(gate)


class CheckoutFixture:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)

    def close(self) -> None:
        self.temporary.cleanup()

    def write(self, relative: str, content: str) -> Path:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        return path

    def workspace(self, members: list[str]) -> None:
        rendered = ", ".join(json.dumps(member) for member in members)
        self.write("Cargo.toml", f"[workspace]\nmembers = [{rendered}]\n")

    def package(self, root: str, name: str, source: str = "fn main() {}\n") -> None:
        self.write(
            f"{root}/Cargo.toml",
            f'[package]\nname = "{name}"\nversion = "0.0.0"\nedition = "2021"\n',
        )
        self.write(f"{root}/src/lib.rs", source)


def package(name: str = "big", root: str = "crates/big") -> gate.Package:
    return gate.Package(name=name, manifest=f"{root}/Cargo.toml", root=root)


def measurement(count: int, name: str = "big", root: str = "crates/big") -> gate.Measurement:
    return gate.Measurement(package=package(name, root), cloc=count, files=1)


class PhysicalCensusTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = CheckoutFixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def test_counts_every_rust_file_beneath_package_root(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        for path in (
            "crates/pkg/build.rs",
            "crates/pkg/src/tests.rs",
            "crates/pkg/tests/integration.rs",
            "crates/pkg/examples/demo.rs",
            "crates/pkg/benches/bench.rs",
            "crates/pkg/dead/conditional.rs",
        ):
            self.fixture.write(path, "// comment\nfn counted() {}\n")

        measured = gate.live_measurements(self.fixture.root)

        self.assertEqual([(item.package.name, item.files, item.cloc) for item in measured], [("pkg", 7, 7)])

    def test_untracked_rust_file_is_seen_without_git(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        self.fixture.write("crates/pkg/scratch/untracked.rs", "fn untracked() {}\n")

        measured = gate.live_measurements(self.fixture.root)

        self.assertEqual(measured[0].files, 2)
        self.assertEqual(measured[0].cloc, 2)

    def test_orphan_rust_file_is_rejected(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        self.fixture.write("scratch/orphan.rs", "fn orphan() {}\n")

        with self.assertRaisesRegex(gate.GateError, r"exactly one workspace package: scratch/orphan\.rs"):
            gate.live_measurements(self.fixture.root)

    def test_undeclared_manifest_is_rejected(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        self.fixture.write("other/Cargo.toml", '[package]\nname = "hidden"\nversion = "0.0.0"\n')

        with self.assertRaisesRegex(gate.GateError, r"undeclared Cargo\.toml.*other/Cargo\.toml"):
            gate.live_measurements(self.fixture.root)

    def test_nested_package_roots_are_rejected(self) -> None:
        self.fixture.workspace(["crates/pkg", "crates/pkg/nested"])
        self.fixture.package("crates/pkg", "pkg")
        self.fixture.package("crates/pkg/nested", "nested")

        with self.assertRaisesRegex(gate.GateError, "overlapping or nested workspace package roots"):
            gate.workspace_packages(self.fixture.root)

    def test_symlinked_rust_file_is_rejected(self) -> None:
        if not hasattr(os, "symlink"):
            self.skipTest("symlinks unavailable")
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        target = self.fixture.write("outside.fixture", "fn linked() {}\n")
        os.symlink(target, self.fixture.root / "crates/pkg/src/linked.rs")

        with self.assertRaisesRegex(gate.GateError, r"symlinked Rust file.*linked\.rs"):
            gate.live_measurements(self.fixture.root)

    def test_package_internal_target_and_node_modules_are_counted(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        self.fixture.write("crates/pkg/target/generated.rs", "fn generated() {}\n")
        self.fixture.write("crates/pkg/node_modules/vendor.rs", "fn vendored() {}\n")

        measured = gate.live_measurements(self.fixture.root)

        self.assertEqual((measured[0].files, measured[0].cloc), (3, 3))

    def test_package_internal_cache_and_vcs_named_directories_are_counted(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        names = (
            ".git",
            ".hg",
            ".buildkite-cache",
            ".mypy_cache",
            ".pytest_cache",
            ".ruff_cache",
            ".svn",
            "__pycache__",
        )
        for index, name in enumerate(names):
            self.fixture.write(f"crates/pkg/{name}/hidden_{index}.rs", "fn hidden() {}\n")

        measured = gate.live_measurements(self.fixture.root)

        self.assertEqual((measured[0].files, measured[0].cloc), (1 + len(names), 1 + len(names)))

    def test_hidden_manifest_in_package_artifact_directory_is_rejected(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        self.fixture.write(
            "crates/pkg/target/hidden/Cargo.toml",
            '[package]\nname = "hidden"\nversion = "0.0.0"\n',
        )

        with self.assertRaisesRegex(
            gate.GateError,
            r"undeclared Cargo\.toml.*crates/pkg/target/hidden/Cargo\.toml",
        ):
            gate.live_measurements(self.fixture.root)

    def test_package_internal_directory_symlinks_are_rejected_regardless_of_name(self) -> None:
        if not hasattr(os, "symlink"):
            self.skipTest("symlinks unavailable")
        for name in (
            "ordinary",
            "target",
            "node_modules",
            ".buildkite-cache",
            ".git",
            ".pytest_cache",
        ):
            with self.subTest(name=name):
                fixture = CheckoutFixture()
                try:
                    fixture.workspace(["crates/pkg"])
                    fixture.package("crates/pkg", "pkg")
                    target = fixture.root / "outside"
                    target.mkdir()
                    os.symlink(target, fixture.root / "crates/pkg" / name)
                    expected = re.escape(f"crates/pkg/{name}")

                    with self.assertRaisesRegex(
                        gate.GateError,
                        rf"symlinked package directory.*{expected}",
                    ):
                        gate.live_measurements(fixture.root)
                finally:
                    fixture.close()

    def test_checkout_level_artifact_directories_remain_pruned(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        names = (*sorted(gate.EXCLUDED_DIRECTORY_NAMES), ".buildkite-cache", "bazel-out")
        for index, name in enumerate(names):
            self.fixture.write(f"{name}/ignored_{index}.rs", "fn generated() {}\n")
            self.fixture.write(
                f"{name}/hidden-{index}/Cargo.toml",
                '[package]\nname = "ignored"\nversion = "0.0.0"\n',
            )

        measured = gate.live_measurements(self.fixture.root)

        self.assertEqual((measured[0].files, measured[0].cloc), (1, 1))

    def test_checkout_level_buildkite_cache_symlink_is_pruned(self) -> None:
        if not hasattr(os, "symlink"):
            self.skipTest("symlinks unavailable")
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        with tempfile.TemporaryDirectory() as cache_name:
            cache = Path(cache_name)
            (cache / "hidden.rs").write_text("fn cached() {}\n", encoding="utf-8")
            (cache / "Cargo.toml").write_text(
                '[package]\nname = "cached"\nversion = "0.0.0"\n',
                encoding="utf-8",
            )
            os.symlink(cache, self.fixture.root / ".buildkite-cache")

            measured = gate.live_measurements(self.fixture.root)

        self.assertEqual((measured[0].files, measured[0].cloc), (1, 1))


class CounterTests(unittest.TestCase):
    def test_metric_counts_code_and_ignores_comment_only_lines(self) -> None:
        content = br'''// line comment
fn one() {}
/* outer
   /* nested */
*/
let ordinary = "// text, not a comment";
let raw = r#"/* text
still string */"#;
let quote = '"';
let byte = b'/';
let lifetime: &'static str = "ok";
/* leading */ fn two() {} // trailing
'''
        self.assertEqual(gate.rust_cloc(content), 8)

    def test_metric_rejects_malformed_utf8_and_unterminated_lexemes(self) -> None:
        with self.assertRaisesRegex(gate.GateError, "not UTF-8"):
            gate.rust_cloc(b"\xff")
        with self.assertRaisesRegex(gate.GateError, "unterminated block comment"):
            gate.rust_cloc(b"/*")
        with self.assertRaisesRegex(gate.GateError, "unterminated string literal"):
            gate.rust_cloc(b'let value = "unterminated')


class LimitTests(unittest.TestCase):
    def test_hard_limit_is_the_only_admission_threshold(self) -> None:
        self.assertEqual(gate.measurement_failures([measurement(21_000)]), [])
        self.assertEqual(
            gate.measurement_failures(
                [measurement(21_001, "new", "crates/new")]
            ),
            ["package=new count=21001 limit=21000"],
        )

    def test_all_over_limit_packages_are_reported_without_state(self) -> None:
        failures = gate.measurement_failures(
            [
                measurement(21_002, "zeta", "crates/zeta"),
                measurement(20_999, "small", "crates/small"),
                measurement(21_001, "alpha", "crates/alpha"),
            ]
        )
        self.assertEqual(
            failures,
            [
                "package=alpha count=21001 limit=21000",
                "package=zeta count=21002 limit=21000",
            ],
        )
        message = gate.format_failures(failures)
        self.assertNotIn("ratchet", message)
        self.assertNotIn("ledger", message)
        self.assertNotIn("admission_sha", message)


class TemporaryGitCheckout:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.run("init", "-q", "-b", "main")
        self.run("config", "user.email", "crate-gate@example.invalid")
        self.run("config", "user.name", "Crate Gate Test")
        self.run("config", "commit.gpgsign", "false")

    def close(self) -> None:
        self.temporary.cleanup()

    def run(self, *arguments: str) -> str:
        result = subprocess.run(
            ["git", *arguments],
            cwd=self.root,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            text=True,
        )
        if result.returncode:
            raise AssertionError(f"git {' '.join(arguments)} failed: {result.stderr}")
        return result.stdout.strip()

    def write(self, relative: str, content: str) -> None:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def commit(self, message: str) -> str:
        self.run("add", "-A")
        self.run("commit", "-q", "-m", message)
        return self.run("rev-parse", "HEAD")

    def base_commit(self) -> str:
        self.write("marker", "base\n")
        return self.commit("base")


class ExactCandidateTests(unittest.TestCase):
    def setUp(self) -> None:
        self.checkout = TemporaryGitCheckout()

    def tearDown(self) -> None:
        self.checkout.close()

    def test_exact_candidate_accepts_current_clean_head(self) -> None:
        candidate = self.checkout.base_commit()

        gate.verify_exact_candidate(self.checkout.root, candidate)

    def test_exact_candidate_rejects_a_different_checked_out_commit(self) -> None:
        base = self.checkout.base_commit()

        with self.assertRaisesRegex(
            gate.GateError, "exact candidate commit does not match checked-out HEAD"
        ):
            gate.verify_exact_candidate(self.checkout.root, "b" * 40)

        self.assertEqual(self.checkout.run("rev-parse", "HEAD"), base)

    def test_exact_candidate_rejects_a_dirty_checkout(self) -> None:
        candidate = self.checkout.base_commit()
        self.checkout.write("dirty", "not committed\n")

        with self.assertRaisesRegex(gate.GateError, "exact candidate checkout is dirty"):
            gate.verify_exact_candidate(self.checkout.root, candidate)

    def test_exact_candidate_rejects_zero_or_malformed_identity(self) -> None:
        self.checkout.base_commit()
        for identity in ("0" * 40, "A" * 40, "abc"):
            with self.subTest(identity=identity):
                with self.assertRaisesRegex(
                    gate.GateError, "nonzero lowercase 40-hex"
                ):
                    gate.verify_exact_candidate(self.checkout.root, identity)


class PythonCompatibilityTests(unittest.TestCase):
    def test_checker_uses_python_310_syntax_and_declared_tomli(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        tree = ast.parse(source, filename=str(SCRIPT), feature_version=(3, 10))
        toml_imports = [
            (alias.name, alias.asname)
            for node in ast.walk(tree)
            if isinstance(node, ast.Import)
            for alias in node.names
            if alias.name in {"tomli", "tomllib"}
        ]
        self.assertEqual(toml_imports, [("tomli", "tomllib")])
        self.assertNotIn("sys.version_info", source)


if __name__ == "__main__":
    unittest.main()
