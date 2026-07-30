#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "check-provider-support-matrix.py"
SPEC = importlib.util.spec_from_file_location("provider_support_matrix", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load provider support matrix validator")
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)


class ProviderSupportMatrixTest(unittest.TestCase):
    def test_repository_claim_scan_inventory_is_current(self) -> None:
        paths = checker.codex_public_claim_scan_paths()
        self.assertEqual(
            [path.name for path in paths],
            ["hydration.rs", "lifecycle.rs", "projection.rs"],
        )
        for path in paths:
            self.assertIn("#[test]", path.read_text(encoding="utf-8"))

        self.assertIn(
            "crates/ctx-cli/tests/support/native_providers/workspace_sources.rs",
            checker.PUBLIC_COVERAGE_PATHS,
        )
        checker.validate_public_claim_docs()

    def test_missing_suite_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            missing = Path(directory) / "missing"
            with self.assertRaisesRegex(
                checker.MatrixError,
                "public claim test suite does not exist",
            ):
                checker.codex_public_claim_scan_paths(missing)

    def test_empty_suite_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            empty = Path(directory) / "empty"
            empty.mkdir()
            with self.assertRaisesRegex(
                checker.MatrixError,
                "public claim test suite contains no Rust sources",
            ):
                checker.codex_public_claim_scan_paths(empty)

    def test_suite_discovery_is_sorted_and_rust_only(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            suite = Path(directory)
            (suite / "b.rs").write_text("#[test]\nfn b() {}\n", encoding="utf-8")
            (suite / "a.rs").write_text("#[test]\nfn a() {}\n", encoding="utf-8")
            (suite / "ignored.txt").write_text("ignored\n", encoding="utf-8")
            self.assertEqual(
                [
                    path.name
                    for path in checker.codex_public_claim_scan_paths(suite)
                ],
                ["a.rs", "b.rs"],
            )


if __name__ == "__main__":
    unittest.main()
