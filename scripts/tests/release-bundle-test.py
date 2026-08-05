#!/usr/bin/env python3
"""Focused regressions for sealing and committing Linux release bundles."""

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
    raise RuntimeError("could not load release bundle utility")
BUNDLE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUNDLE)


class ReleaseBundleTests(unittest.TestCase):
    platform = "linux-x64"
    source_commit = "a" * 40

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.output = self.root / "publish/candidate"
        self.output.parent.mkdir()
        self.stage = self.output.parent / ".ctx-release-stage"
        self._make_stage(self.stage)
        self.symbols = self.root / "symbols-source"
        (self.symbols / "nested").mkdir(parents=True)
        (self.symbols / "manifest.json").write_bytes(b"{}\n")
        (self.symbols / "nested/ctx.debug").write_bytes(b"symbols\n")
        self.symbols_output = self.root / "private/symbols"
        self.symbols_output.parent.mkdir()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _make_stage(self, destination: Path) -> str:
        destination.mkdir()
        self._populate_stage(destination)
        return BUNDLE.seal_bundle(destination, self.platform, self.source_commit)

    def _populate_stage(self, destination: Path) -> None:
        for name in BUNDLE.expected_release_leaves(self.platform):
            path = destination / name
            path.write_bytes(f"candidate leaf {name}\n".encode())
            path.chmod(0o755 if name == "ctx" else 0o644)

    def seal(self, stage: Path | None = None) -> str:
        target = self.stage if stage is None else stage
        marker = target / BUNDLE.completion_leaf(self.platform)
        return __import__("hashlib").sha256(marker.read_bytes()).hexdigest()

    def commit(self, phase_hook=None) -> None:
        BUNDLE.commit_bundle(
            self.stage,
            self.output,
            self.symbols,
            self.symbols_output,
            self.platform,
            self.source_commit,
            seal_sha256=self.seal(),
            phase_hook=phase_hook,
        )

    def test_seal_preserves_schema_and_exact_leaf_digests(self) -> None:
        payload = BUNDLE.verify_bundle(
            self.stage,
            self.platform,
            self.source_commit,
            seal_sha256=self.seal(),
        )
        self.assertEqual(payload["kind"], "ctx-public-linux-release-completion")
        self.assertEqual(
            [record["name"] for record in payload["files"]],
            BUNDLE.expected_release_leaves(self.platform),
        )

    def test_seal_fsyncs_every_leaf_marker_and_stage(self) -> None:
        stage = self.root / "durable-stage"
        stage.mkdir()
        self._populate_stage(stage)
        with mock.patch.object(BUNDLE.os, "fsync", wraps=os.fsync) as fsync:
            BUNDLE.seal_bundle(stage, self.platform, self.source_commit)
        self.assertEqual(
            fsync.call_count,
            len(BUNDLE.expected_release_leaves(self.platform)) + 2,
        )

    def test_resolve_output_is_safe_to_eval_with_spaces(self) -> None:
        output = "target/release candidate"
        symbols = self.root / "private symbols"
        resolved = subprocess.run(
            [
                sys.executable,
                str(MODULE_PATH),
                "resolve",
                "--repo-root",
                str(self.repo),
                "--output-dir",
                output,
                "--private-symbols-dir",
                str(symbols),
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        evaluated = subprocess.run(
            [
                "bash",
                "-c",
                resolved.stdout
                + "printf '%s\\n%s\\n' \"$CTX_LINUX_RELEASE_OUTPUT_DIR\" "
                + "\"$CTX_LINUX_RELEASE_PRIVATE_SYMBOLS_DIR\"",
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            evaluated.stdout.splitlines(),
            [str(self.repo / output), str(symbols)],
        )

    def test_verifier_rejects_missing_extra_link_and_changed_bytes(self) -> None:
        cases = []
        missing = self.root / "missing"
        self._make_stage(missing)
        (missing / "ctx.sha256").unlink()
        cases.append(missing)

        extra = self.root / "extra"
        self._make_stage(extra)
        (extra / "unexpected").write_text("extra\n")
        cases.append(extra)

        linked = self.root / "linked"
        self._make_stage(linked)
        (linked / "ctx.sha256").unlink()
        (linked / "ctx.sha256").symlink_to(self.root / "outside")
        cases.append(linked)

        changed = self.root / "changed"
        self._make_stage(changed)
        (changed / "ctx").write_bytes(b"changed\n")
        cases.append(changed)

        for candidate in cases:
            with self.subTest(candidate=candidate.name):
                with self.assertRaises((BUNDLE.BundleError, FileNotFoundError)):
                    BUNDLE.verify_bundle(candidate, self.platform, self.source_commit)

    def test_verifier_rejects_wrong_source_and_wrong_smoked_seal(self) -> None:
        with self.assertRaisesRegex(BUNDLE.BundleError, "identity"):
            BUNDLE.verify_bundle(self.stage, self.platform, "b" * 40)
        with self.assertRaisesRegex(BUNDLE.BundleError, "smoked candidate"):
            BUNDLE.verify_bundle(
                self.stage,
                self.platform,
                self.source_commit,
                seal_sha256="0" * 64,
            )

    def test_allow_extra_still_rejects_non_regular_aggregate_leaves(self) -> None:
        (self.stage / "semantic-asset").write_text("asset\n")
        BUNDLE.verify_bundle(
            self.stage, self.platform, self.source_commit, allow_extra=True
        )
        (self.stage / "directory").mkdir()
        with self.assertRaisesRegex(BUNDLE.BundleError, "non-regular"):
            BUNDLE.verify_bundle(
                self.stage, self.platform, self.source_commit, allow_extra=True
            )

    def test_commit_renames_exact_stage_and_preserves_recursive_symbols(self) -> None:
        expected_ctx = (self.stage / "ctx").read_bytes()
        with mock.patch.object(
            BUNDLE, "_fsync_directory", wraps=BUNDLE._fsync_directory
        ) as sync_directory:
            self.commit()
        self.assertIn(
            mock.call(self.symbols_output.parent), sync_directory.call_args_list
        )
        self.assertIn(mock.call(self.output.parent), sync_directory.call_args_list)
        self.assertFalse(self.stage.exists())
        self.assertEqual((self.output / "ctx").read_bytes(), expected_ctx)
        self.assertEqual((self.output / "ctx").stat().st_mode & 0o777, 0o755)
        self.assertEqual(
            (self.symbols_output / "nested/ctx.debug").read_bytes(), b"symbols\n"
        )
        BUNDLE.verify_bundle(
            self.output,
            self.platform,
            self.source_commit,
            seal_sha256=self.seal(self.output),
        )

    def test_generic_directory_commit_fsyncs_destination_parent(self) -> None:
        stage = self.output.parent / ".generic-stage"
        stage.mkdir()
        (stage / "asset").write_text("asset\n")
        output = self.output.parent / "generic-output"
        with mock.patch.object(
            BUNDLE, "_fsync_directory", wraps=BUNDLE._fsync_directory
        ) as sync_directory:
            BUNDLE.commit_directory(stage, output)
        sync_directory.assert_called_once_with(output.parent)

    def test_public_collision_rolls_back_private_commit(self) -> None:
        def collide(phase: str) -> None:
            if phase == "before-public-commit":
                self.output.mkdir()
                (self.output / "sentinel").write_text("sentinel\n")

        with self.assertRaisesRegex(BUNDLE.BundleError, "already exists"):
            self.commit(collide)
        self.assertEqual((self.output / "sentinel").read_text(), "sentinel\n")
        self.assertFalse(self.symbols_output.exists())
        self.assertTrue(self.stage.is_dir())

    def test_symbol_collision_happens_before_public_commit(self) -> None:
        def collide(phase: str) -> None:
            if phase == "before-symbol-commit":
                self.symbols_output.mkdir()
                (self.symbols_output / "sentinel").write_text("sentinel\n")

        with self.assertRaisesRegex(BUNDLE.BundleError, "already exists"):
            self.commit(collide)
        self.assertFalse(self.output.exists())
        self.assertEqual(
            (self.symbols_output / "sentinel").read_text(), "sentinel\n"
        )

    def test_commit_rejects_stage_outside_destination_parent(self) -> None:
        other = self.root / "other/stage"
        other.parent.mkdir()
        seal = self._make_stage(other)
        with self.assertRaisesRegex(BUNDLE.BundleError, "sibling"):
            BUNDLE.commit_bundle(
                other,
                self.output,
                self.symbols,
                self.symbols_output,
                self.platform,
                self.source_commit,
                seal_sha256=seal,
            )

    def test_commit_rechecks_stage_against_pre_smoke_seal(self) -> None:
        original_seal = self.seal()
        replacement = self.output.parent / ".replacement"
        self._make_stage(replacement)
        marker = replacement / BUNDLE.completion_leaf(self.platform)
        marker.unlink()
        (replacement / "ctx").write_bytes(b"different candidate\n")
        (replacement / "ctx").chmod(0o755)
        BUNDLE.seal_bundle(replacement, self.platform, self.source_commit)
        original = self.output.parent / ".original"

        def substitute(phase: str) -> None:
            if phase == "after-verification":
                os.rename(self.stage, original)
                os.rename(replacement, self.stage)

        with self.assertRaisesRegex(BUNDLE.BundleError, "smoked candidate"):
            BUNDLE.commit_bundle(
                self.stage,
                self.output,
                self.symbols,
                self.symbols_output,
                self.platform,
                self.source_commit,
                seal_sha256=original_seal,
                phase_hook=substitute,
            )
        self.assertFalse(self.output.exists())
        self.assertFalse(self.symbols_output.exists())

    def test_recursive_symbol_link_is_rejected_without_touching_target(self) -> None:
        outside = self.root / "outside"
        outside.write_text("sentinel\n")
        (self.symbols / "nested/link").symlink_to(outside)
        with self.assertRaisesRegex(BUNDLE.BundleError, "regular file"):
            self.commit()
        self.assertEqual(outside.read_text(), "sentinel\n")
        self.assertFalse(self.output.exists())

    def test_preflight_resolves_defaults_and_rejects_symlinked_parent(self) -> None:
        output, symbols = BUNDLE.preflight_destinations(
            self.repo, "target/release", None
        )
        self.assertEqual(output, self.repo / "target/release")
        self.assertEqual(symbols, self.repo / "target/release.private-debug-symbols")
        outside = self.root / "outside-parent"
        outside.mkdir()
        (self.repo / "linked").symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(BUNDLE.BundleError, "link or file"):
            BUNDLE.preflight_destinations(
                self.repo,
                "linked/candidate",
                str(self.root / "private/other"),
            )
        self.assertEqual(list(outside.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
