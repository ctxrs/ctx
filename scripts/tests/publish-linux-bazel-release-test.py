#!/usr/bin/env python3
"""Executable regressions for transactional Linux release publication."""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "scripts/release/publish-linux-bazel-release.py"
SPEC = importlib.util.spec_from_file_location("publish_linux_bazel_release", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load Linux release publisher")
PUBLISHER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PUBLISHER)


class LinuxReleasePublicationTests(unittest.TestCase):
    platform = "linux-x64"
    source_commit = "a" * 40

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.artifact_source = self.root / "artifact-source"
        self._make_artifact_source(self.artifact_source)
        self.symbols_source = self.root / "symbols-source"
        self.symbols_source.mkdir()
        (self.symbols_source / "manifest.json").write_bytes(b"{}\n")
        (self.symbols_source / "symbols.tar.gz").write_bytes(b"symbols\n")
        for path in self.symbols_source.iterdir():
            path.chmod(0o600)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _make_artifact_source(self, destination: Path) -> None:
        destination.mkdir()
        for name in PUBLISHER.expected_release_leaves(self.platform):
            path = destination / name
            path.write_bytes(f"candidate leaf {name}\n".encode())
            path.chmod(0o755 if name == "ctx" else 0o644)
        PUBLISHER.seal_candidate(destination, self.platform, self.source_commit)

    @staticmethod
    def assert_sentinel(path: Path) -> None:
        if path.read_bytes() != b"sentinel\n":
            raise AssertionError(f"external sentinel changed: {path}")

    def preflight(self, output: str, symbols: str | None) -> tuple[Path, Path]:
        return PUBLISHER.preflight_destinations(self.repo, output, symbols)

    def publish(self, output: Path, symbols: Path, phase_hook=None) -> None:
        PUBLISHER.publish(
            self.artifact_source,
            output,
            self.symbols_source,
            symbols,
            self.platform,
            self.source_commit,
            phase_hook=phase_hook,
        )

    def assert_candidate_rejected(self, path: Path) -> None:
        with self.assertRaises((PUBLISHER.PublicationError, OSError)):
            PUBLISHER.verify_candidate(path, self.platform, self.source_commit)

    def test_relative_output_resolves_before_default_symbols(self) -> None:
        output, symbols = self.preflight("target/public-cli-artifacts", None)
        self.assertEqual(output, self.repo / "target/public-cli-artifacts")
        self.assertEqual(
            symbols,
            self.repo / "target/public-cli-artifacts.private-debug-symbols",
        )
        self.assertTrue(output.parent.is_dir())
        self.assertFalse(output.exists())
        self.assertFalse(symbols.exists())

    def test_explicit_external_absolute_destinations_are_preserved(self) -> None:
        output_argument = self.root / "external/public"
        symbols_argument = self.root / "private/symbols"
        output, symbols = self.preflight(str(output_argument), str(symbols_argument))
        self.assertEqual(output, output_argument)
        self.assertEqual(symbols, symbols_argument)
        self.publish(output, symbols)
        PUBLISHER.verify_candidate(output, self.platform, self.source_commit)

    def test_symlinked_ignored_output_ancestor_is_rejected_before_writes(self) -> None:
        external = self.root / "external-output"
        external.mkdir()
        sentinel = external / "sentinel"
        sentinel.write_bytes(b"sentinel\n")
        (self.repo / "target").symlink_to(external, target_is_directory=True)
        symbols = self.root / "private/symbols"
        with self.assertRaisesRegex(PUBLISHER.PublicationError, "symlink"):
            self.preflight("target/public-cli-artifacts", str(symbols))
        self.assert_sentinel(sentinel)
        self.assertEqual(sorted(path.name for path in external.iterdir()), ["sentinel"])
        self.assertFalse(symbols.parent.exists())

    def test_symlinked_symbol_ancestor_is_rejected_before_writes(self) -> None:
        external = self.root / "external-symbols"
        external.mkdir()
        sentinel = external / "sentinel"
        sentinel.write_bytes(b"sentinel\n")
        symbol_link = self.root / "private"
        symbol_link.symlink_to(external, target_is_directory=True)
        output = self.root / "public/candidate"
        with self.assertRaisesRegex(PUBLISHER.PublicationError, "symlink"):
            self.preflight(str(output), str(symbol_link / "symbols"))
        self.assert_sentinel(sentinel)
        self.assertFalse(output.exists())
        self.assertFalse(output.parent.exists())

    def test_final_public_link_is_rejected_without_following_it(self) -> None:
        output_parent = self.root / "public"
        output_parent.mkdir()
        external = self.root / "external"
        external.mkdir()
        sentinel = external / "sentinel"
        sentinel.write_bytes(b"sentinel\n")
        (output_parent / "candidate").symlink_to(external, target_is_directory=True)
        with self.assertRaisesRegex(PUBLISHER.PublicationError, "already exists"):
            self.preflight(
                str(output_parent / "candidate"), str(self.root / "private/symbols")
            )
        self.assert_sentinel(sentinel)

    def test_final_symbol_link_is_rejected_without_following_it(self) -> None:
        symbols_parent = self.root / "private"
        symbols_parent.mkdir()
        external = self.root / "external-symbols"
        external.mkdir()
        symbols = symbols_parent / "symbols"
        symbols.symlink_to(external, target_is_directory=True)
        with self.assertRaisesRegex(PUBLISHER.PublicationError, "already exists"):
            self.preflight(str(self.root / "public/candidate"), str(symbols))
        self.assertEqual(list(external.iterdir()), [])

    def test_preexisting_final_directory_is_never_replaced(self) -> None:
        output = self.root / "public/candidate"
        output.mkdir(parents=True)
        sentinel = output / "sentinel"
        sentinel.write_bytes(b"sentinel\n")
        with self.assertRaisesRegex(PUBLISHER.PublicationError, "already exists"):
            self.preflight(str(output), str(self.root / "private/symbols"))
        self.assert_sentinel(sentinel)

    def test_publication_preserves_complete_bundle_and_modes(self) -> None:
        output, symbols = self.preflight(
            str(self.root / "public/candidate"), str(self.root / "private/symbols")
        )
        self.publish(output, symbols)
        PUBLISHER.verify_candidate(output, self.platform, self.source_commit)
        self.assertEqual((output / "ctx").stat().st_mode & 0o777, 0o755)
        self.assertEqual((output / "ctx.sha256").stat().st_mode & 0o777, 0o644)
        self.assertEqual((symbols / "manifest.json").read_bytes(), b"{}\n")
        self.assertEqual((symbols / "symbols.tar.gz").read_bytes(), b"symbols\n")

    def test_symbol_collision_fails_before_any_public_candidate(self) -> None:
        output, symbols = self.preflight(
            str(self.root / "public/candidate"), str(self.root / "private/symbols")
        )

        def collide(phase: str) -> None:
            if phase == "before-symbol-commit":
                symbols.mkdir()
                (symbols / "sentinel").write_bytes(b"sentinel\n")

        with self.assertRaisesRegex(PUBLISHER.PublicationError, "already exists"):
            self.publish(output, symbols, collide)
        self.assertFalse(output.exists())
        self.assert_sentinel(symbols / "sentinel")

    def test_collision_after_symbol_commit_is_downstream_ineligible(self) -> None:
        output, symbols = self.preflight(
            str(self.root / "public/candidate"), str(self.root / "private/symbols")
        )

        def collide(phase: str) -> None:
            if phase == "after-symbol-commit":
                output.mkdir()
                (output / "sentinel").write_bytes(b"sentinel\n")

        with self.assertRaisesRegex(PUBLISHER.PublicationError, "already exists"):
            self.publish(output, symbols, collide)
        self.assertTrue(symbols.is_dir())
        self.assert_sentinel(output / "sentinel")
        self.assert_candidate_rejected(output)

    def test_decisive_collision_after_final_check_is_no_replace(self) -> None:
        output, symbols = self.preflight(
            str(self.root / "public/candidate"), str(self.root / "private/symbols")
        )

        def collide(phase: str) -> None:
            if phase == "before-public-rename":
                output.mkdir()
                (output / "sentinel").write_bytes(b"sentinel\n")

        with self.assertRaisesRegex(PUBLISHER.PublicationError, "appeared"):
            self.publish(output, symbols, collide)
        self.assertTrue(symbols.is_dir())
        self.assert_sentinel(output / "sentinel")
        self.assert_candidate_rejected(output)

    def test_parent_substitution_after_symbols_fails_before_public_commit(self) -> None:
        output, symbols = self.preflight(
            str(self.root / "public/candidate"), str(self.root / "private/symbols")
        )
        verified_parent = self.root / "verified-public"
        external = self.root / "external"
        external.mkdir()
        sentinel = external / "sentinel"
        sentinel.write_bytes(b"sentinel\n")

        def substitute(phase: str) -> None:
            if phase == "after-symbol-commit":
                os.rename(output.parent, verified_parent)
                output.parent.symlink_to(external, target_is_directory=True)

        with self.assertRaisesRegex(PUBLISHER.PublicationError, "substituted"):
            self.publish(output, symbols, substitute)
        self.assert_sentinel(sentinel)
        self.assertFalse((verified_parent / output.name).exists())
        self.assert_candidate_rejected(output)

    def test_parent_substitution_after_public_commit_cannot_report_success(self) -> None:
        output, symbols = self.preflight(
            str(self.root / "public/candidate"), str(self.root / "private/symbols")
        )
        verified_parent = self.root / "verified-public"
        external = self.root / "external"
        external.mkdir()
        sentinel = external / "sentinel"
        sentinel.write_bytes(b"sentinel\n")

        def substitute(phase: str) -> None:
            if phase == "after-public-commit":
                os.rename(output.parent, verified_parent)
                output.parent.symlink_to(external, target_is_directory=True)

        with self.assertRaisesRegex(PUBLISHER.PublicationError, "substituted"):
            self.publish(output, symbols, substitute)
        self.assert_sentinel(sentinel)
        self.assert_candidate_rejected(output)
        self.assertFalse((verified_parent / output.name).exists())
        self.assertFalse(
            any(
                path.name.startswith(".ctx-release-publish")
                for path in verified_parent.iterdir()
            )
        )

    def test_obstructed_post_commit_rollback_invalidates_candidate(self) -> None:
        output, symbols = self.preflight(
            str(self.root / "public/candidate"), str(self.root / "private/symbols")
        )
        verified_parent = self.root / "verified-public"
        external = self.root / "external"
        external.mkdir()
        sentinel = external / "sentinel"
        sentinel.write_bytes(b"sentinel\n")
        stage_name = ""

        def substitute_and_obstruct(phase: str) -> None:
            nonlocal stage_name
            if phase == "before-symbol-commit":
                matches = [
                    path.name
                    for path in output.parent.iterdir()
                    if path.name.startswith(".ctx-release-publish")
                ]
                self.assertEqual(len(matches), 1)
                stage_name = matches[0]
            elif phase == "after-public-commit":
                os.rename(output.parent, verified_parent)
                output.parent.symlink_to(external, target_is_directory=True)
                obstruction = verified_parent / stage_name
                obstruction.mkdir()
                (obstruction / "sentinel").write_bytes(b"sentinel\n")

        with self.assertRaisesRegex(PUBLISHER.PublicationError, "invalidated"):
            self.publish(output, symbols, substitute_and_obstruct)
        self.assert_sentinel(sentinel)
        self.assert_sentinel(verified_parent / stage_name / "sentinel")
        self.assert_candidate_rejected(output)
        self.assert_candidate_rejected(verified_parent / output.name)

    def test_completed_leaf_link_is_rejected_without_touching_sentinel(self) -> None:
        external = self.root / "external-file"
        external.write_bytes(b"sentinel\n")
        leaf = self.artifact_source / "ctx.sha256"
        leaf.unlink()
        leaf.symlink_to(external)
        output, symbols = self.preflight(
            str(self.root / "public/candidate"), str(self.root / "private/symbols")
        )
        with self.assertRaises((PUBLISHER.PublicationError, OSError)):
            self.publish(output, symbols)
        self.assert_sentinel(external)
        self.assertFalse(output.exists())
        self.assertFalse(symbols.exists())

    def test_cross_filesystem_source_is_copied_before_atomic_commit(self) -> None:
        shared_memory = Path("/dev/shm")
        if not shared_memory.is_dir() or shared_memory.stat().st_dev == self.root.stat().st_dev:
            self.skipTest("distinct /dev/shm filesystem is unavailable")
        foreign_root = Path(tempfile.mkdtemp(prefix="ctx-release-test.", dir=shared_memory))
        self.addCleanup(shutil.rmtree, foreign_root)
        foreign_source = foreign_root / "candidate"
        self._make_artifact_source(foreign_source)
        output, symbols = self.preflight(
            str(self.root / "public/candidate"), str(self.root / "private/symbols")
        )
        PUBLISHER.publish(
            foreign_source,
            output,
            self.symbols_source,
            symbols,
            self.platform,
            self.source_commit,
        )
        PUBLISHER.verify_candidate(output, self.platform, self.source_commit)

    def test_snapshot_rejects_parent_substitution_and_preserves_external(self) -> None:
        output, symbols = self.preflight(
            str(self.root / "public/candidate"), str(self.root / "private/symbols")
        )
        self.publish(output, symbols)
        verified_parent = self.root / "verified-public"
        external = self.root / "external"
        external.mkdir()
        sentinel = external / "sentinel"
        sentinel.write_bytes(b"sentinel\n")

        def substitute() -> None:
            os.rename(output.parent, verified_parent)
            output.parent.symlink_to(external, target_is_directory=True)

        with self.assertRaisesRegex(PUBLISHER.PublicationError, "substituted"):
            PUBLISHER.snapshot_candidates(
                output,
                self.root,
                [self.platform],
                self.source_commit,
                before_finish=substitute,
            )
        self.assert_sentinel(sentinel)
        self.assertFalse(any(path.name.startswith(".ctx-release-snapshot") for path in self.root.iterdir()))

    def test_run_handoff_uses_verified_descriptor_and_fails_on_substitution(self) -> None:
        output, symbols = self.preflight(
            str(self.root / "public/candidate"), str(self.root / "private/symbols")
        )
        self.publish(output, symbols)
        verified_parent = self.root / "verified-public"
        external = self.root / "external"
        external.mkdir()
        sentinel = external / "sentinel"
        sentinel.write_bytes(b"sentinel\n")
        command = [
            sys.executable,
            "-c",
            (
                "import os, pathlib, sys; "
                "candidate=pathlib.Path(sys.argv[1]); "
                "assert (candidate/'ctx').read_bytes().startswith(b'candidate'); "
                "os.rename(sys.argv[2], sys.argv[3]); "
                "os.symlink(sys.argv[4], sys.argv[2], target_is_directory=True)"
            ),
            "{candidate}",
            str(output.parent),
            str(verified_parent),
            str(external),
        ]
        with self.assertRaisesRegex(PUBLISHER.PublicationError, "substituted"):
            PUBLISHER.run_complete_candidate(
                output, self.platform, self.source_commit, command
            )
        self.assert_sentinel(sentinel)

    def test_task_cleanup_removes_only_expected_identity(self) -> None:
        work_root = self.root / "work"
        task_root = work_root / "task"
        (task_root / "nested").mkdir(parents=True)
        (task_root / "nested/file").write_bytes(b"staged\n")
        identity = task_root.stat()
        PUBLISHER.cleanup_task_root(
            work_root, task_root, identity.st_dev, identity.st_ino
        )
        self.assertFalse(task_root.exists())

    def test_concurrent_task_parent_substitution_fails_without_external_cleanup(self) -> None:
        work_root = self.root / "work"
        task_root = work_root / "task"
        task_root.mkdir(parents=True)
        (task_root / "staged").write_bytes(b"staged\n")
        identity = task_root.stat()
        verified_work_root = self.root / "verified-work"
        external = self.root / "external-work"
        external_task = external / task_root.name
        external_task.mkdir(parents=True)
        sentinel = external_task / "sentinel"
        sentinel.write_bytes(b"sentinel\n")

        def substitute() -> None:
            os.rename(work_root, verified_work_root)
            work_root.symlink_to(external, target_is_directory=True)

        with self.assertRaisesRegex(PUBLISHER.PublicationError, "substituted"):
            PUBLISHER.cleanup_task_root(
                work_root,
                task_root,
                identity.st_dev,
                identity.st_ino,
                before_remove=substitute,
            )
        self.assertTrue((verified_work_root / task_root.name / "staged").is_file())
        self.assert_sentinel(sentinel)

    def test_cleanup_refuses_mount_id_change_before_deleting_anything(self) -> None:
        work_root = self.root / "work"
        nested = work_root / "task/nested"
        nested.mkdir(parents=True)
        first = work_root / "task/first"
        second = nested / "second"
        first.write_bytes(b"first\n")
        second.write_bytes(b"second\n")
        task_root = work_root / "task"
        identity = task_root.stat()
        nested_inode = nested.stat().st_ino
        real_mount_id = PUBLISHER._mount_id

        def mount_id(descriptor: int) -> int:
            if os.fstat(descriptor).st_ino == nested_inode:
                return 424242
            return 313131

        with mock.patch.object(PUBLISHER, "_mount_id", side_effect=mount_id):
            with self.assertRaisesRegex(PUBLISHER.PublicationError, "mount boundary"):
                PUBLISHER.cleanup_task_root(
                    work_root, task_root, identity.st_dev, identity.st_ino
                )
        self.assertEqual(first.read_bytes(), b"first\n")
        self.assertEqual(second.read_bytes(), b"second\n")
        descriptor = os.open(task_root, PUBLISHER.DIRECTORY_FLAGS)
        try:
            self.assertGreater(real_mount_id(descriptor), 0)
        finally:
            os.close(descriptor)


if __name__ == "__main__":
    unittest.main()
