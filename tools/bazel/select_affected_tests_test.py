#!/usr/bin/env python3
"""Mutation matrix for affected-target fail-closed behavior."""

import pathlib
import tempfile
import unittest

from select_affected_tests import FULL_SUITE, select


class SelectionMutationTests(unittest.TestCase):
    def test_full_suite_is_canonical_ci(self) -> None:
        self.assertEqual(FULL_SUITE, "//:ci")

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

    def test_non_routine_targets_are_not_selected(self) -> None:
        changed = self.mutate("crates/ctx-cli/src/main.rs")
        impacted = [
            "//crates/ctx-cli:focused_tests",
            "//crates/ctx-cli:advisory_test",
            "//crates/ctx-cli:external_test",
            "//crates/ctx-cli:external_harness_test",
            "//crates/ctx-cli:flaky_repetition_test",
            "//crates/ctx-cli:manual_smoke",
            "//crates/ctx-cli:network_tests",
            "//crates/ctx-cli:no_cache_test",
            "//crates/ctx-cli:platform_native_test",
            "//crates/ctx-cli:release_contract_tests",
            "//crates/ctx-cli:requires_local_history_test",
            "//crates/ctx-cli:requires_signing_test",
            "//crates/ctx-cli:requires_vm_test",
            "//crates/ctx-cli:stress_test",
            "//release:unit_tests",
        ]
        self.assertEqual(select([changed], impacted), ["//crates/ctx-cli:focused_tests"])

    def test_only_excluded_targets_fails_closed(self) -> None:
        changed = self.mutate("crates/ctx-cli/src/main.rs")
        self.assertEqual(
            select([changed], ["//crates/ctx-cli:release_contract_tests"]),
            [FULL_SUITE],
        )

    def test_no_changes_preserves_empty_focused_selection(self) -> None:
        self.assertEqual(select([], []), [])


if __name__ == "__main__":
    unittest.main()
