#!/usr/bin/env python3
"""Focused regressions for generic atomic release-directory helpers."""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "scripts/release/release_bundle.py"
SPEC = importlib.util.spec_from_file_location("release_bundle", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load release directory utility")
BUNDLE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUNDLE)


class ReleaseDirectoryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.source = self.root / "input"
        self.source.mkdir()
        (self.source / "asset").write_text("asset\n")
        self.parent = self.root / "publication"
        self.parent.mkdir()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def stage(self, name: str = ".stage") -> Path:
        stage = self.parent / name
        (stage / "nested").mkdir(parents=True)
        (stage / "nested/asset").write_text("asset\n")
        return stage

    def test_require_unsealed_rejects_any_completion_marker(self) -> None:
        BUNDLE.require_unsealed(self.source)
        marker = self.source / "ctx-core.release-complete.json"
        marker.write_text("{}\n")
        with self.assertRaisesRegex(BUNDLE.BundleError, "cannot be modified"):
            BUNDLE.require_unsealed(self.source)

    def test_publication_preflight_requires_fresh_distinct_outputs(self) -> None:
        assets = self.parent / "assets"
        authority = self.parent / "authority"
        BUNDLE.preflight_publication_directories(self.source, [assets, authority])
        authority.mkdir()
        with self.assertRaisesRegex(BUNDLE.BundleError, "already exists"):
            BUNDLE.preflight_publication_directories(self.source, [assets, authority])
        with self.assertRaisesRegex(BUNDLE.BundleError, "invalid"):
            BUNDLE.preflight_publication_directories(self.source, [assets, assets])
        with self.assertRaisesRegex(BUNDLE.BundleError, "must not be nested"):
            BUNDLE.preflight_publication_directories(
                self.source, [assets, assets / "authority"]
            )

    def test_commit_directory_makes_the_exact_tree_durable(self) -> None:
        stage = self.stage()
        output = self.parent / "output"
        with mock.patch.object(
            BUNDLE, "_fsync_directory", wraps=BUNDLE._fsync_directory
        ) as sync_directory, mock.patch.object(
            BUNDLE, "_rename_noreplace_at", wraps=BUNDLE._rename_noreplace_at
        ) as rename, mock.patch.object(
            BUNDLE.os, "fsync", wraps=os.fsync
        ) as fsync:
            BUNDLE.commit_directory(stage, output)
        self.assertEqual(
            sync_directory.call_args_list,
            [mock.call(stage / "nested"), mock.call(stage)],
        )
        source_parent, source_leaf, destination_parent, destination_leaf, _ = (
            rename.call_args.args
        )
        self.assertEqual(source_parent, destination_parent)
        self.assertEqual((source_leaf, destination_leaf), (".stage", "output"))
        self.assertIn(mock.call(destination_parent), fsync.call_args_list)
        self.assertEqual((output / "nested/asset").read_text(), "asset\n")
        self.assertFalse(stage.exists())

    def test_commit_rejects_collision_without_modifying_either_tree(self) -> None:
        stage = self.stage()
        output = self.parent / "output"
        output.mkdir()
        (output / "sentinel").write_text("sentinel\n")
        with self.assertRaisesRegex(BUNDLE.BundleError, "already exists"):
            BUNDLE.commit_directory(stage, output)
        self.assertEqual((output / "sentinel").read_text(), "sentinel\n")
        self.assertEqual((stage / "nested/asset").read_text(), "asset\n")

    def test_commit_rejects_non_sibling_and_linked_paths(self) -> None:
        stage = self.stage()
        with self.assertRaisesRegex(BUNDLE.BundleError, "sibling"):
            BUNDLE.commit_directory(stage, self.root / "other/output")

        outside = self.root / "outside"
        outside.mkdir()
        linked_parent = self.root / "linked-parent"
        linked_parent.symlink_to(self.parent, target_is_directory=True)
        with self.assertRaisesRegex(BUNDLE.BundleError, "symlink|non-directory"):
            BUNDLE.commit_directory(
                linked_parent / stage.name, linked_parent / "linked-output"
            )
        self.assertFalse((outside / "linked-output").exists())

    def test_commit_rejects_a_link_inside_the_staged_tree(self) -> None:
        stage = self.stage()
        outside = self.root / "outside-file"
        outside.write_text("sentinel\n")
        (stage / "nested/link").symlink_to(outside)
        with self.assertRaisesRegex(BUNDLE.BundleError, "non-regular"):
            BUNDLE.commit_directory(stage, self.parent / "output")
        self.assertEqual(outside.read_text(), "sentinel\n")

    def test_commit_fails_if_bound_parent_is_detached_after_rename(self) -> None:
        stage = self.stage()
        output = self.parent / "output"
        detached = self.root / "detached-parent"
        rename_noreplace = BUNDLE._rename_noreplace_at

        def detach_after_rename(*args) -> None:
            rename_noreplace(*args)
            os.rename(output.parent, detached)
            output.parent.mkdir()

        with mock.patch.object(
            BUNDLE, "_rename_noreplace_at", side_effect=detach_after_rename
        ), self.assertRaisesRegex(BUNDLE.BundleError, "bound directory"):
            BUNDLE.commit_directory(stage, output)
        self.assertFalse(output.exists())
        self.assertEqual((detached / "output/nested/asset").read_text(), "asset\n")

    def test_require_directory_rejects_a_symlinked_ancestor(self) -> None:
        real = self.root / "real/nested/artifacts"
        real.mkdir(parents=True)
        linked = self.root / "linked"
        linked.symlink_to(self.root / "real", target_is_directory=True)
        checked = subprocess.run(
            [
                sys.executable,
                str(MODULE_PATH),
                "require-directory",
                "--directory",
                str(linked / "nested/artifacts"),
            ],
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(checked.returncode, 0)
        self.assertRegex(checked.stderr, "symlink|non-directory")


if __name__ == "__main__":
    unittest.main()
