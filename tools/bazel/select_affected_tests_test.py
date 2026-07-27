#!/usr/bin/env python3
"""Mutation matrix for affected-target fail-closed behavior."""

import pathlib
import tempfile
import unittest

from select_affected_tests import FULL_SUITE, select


class SelectionMutationTests(unittest.TestCase):
    def mutate(self, relative: str) -> str:
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory, relative)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("before\n")
            path.write_text("after\n")
            self.assertEqual(path.read_text(), "after\n")
        return relative

    def test_source_mutation_selects_native_test(self) -> None:
        changed = self.mutate("crates/ctx-history-core/src/lib.rs")
        self.assertEqual(select([changed], ["//crates/ctx-history-core:unit_tests"]), ["//crates/ctx-history-core:unit_tests"])

    def test_manifest_and_lock_mutations_select_full_suite(self) -> None:
        for path in ("Cargo.toml", "Cargo.lock", "MODULE.bazel", "MODULE.bazel.lock"):
            self.assertEqual(select([self.mutate(path)], ["//crates/ctx-cli:unit_tests"]), [FULL_SUITE])

    def test_build_bzl_and_config_mutations_select_full_suite(self) -> None:
        for path in ("crates/ctx-cli/BUILD.bazel", "tools/bazel/ctx_rust.bzl", ".bazelrc"):
            self.assertEqual(select([self.mutate(path)], ["//crates/ctx-cli:unit_tests"]), [FULL_SUITE])

    def test_diff_failure_or_unmapped_change_selects_full_suite(self) -> None:
        self.assertEqual(select(["README.md"], [], diff_succeeded=False), [FULL_SUITE])
        self.assertEqual(select(["crates/ctx-cli/src/main.rs"], []), [FULL_SUITE])


if __name__ == "__main__":
    unittest.main()
