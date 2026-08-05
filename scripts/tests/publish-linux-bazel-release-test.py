#!/usr/bin/env python3
"""Executable regressions for descriptor-anchored Linux release publication."""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "scripts/release/publish-linux-bazel-release.py"
SPEC = importlib.util.spec_from_file_location("publish_linux_bazel_release", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load Linux release publisher")
PUBLISHER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PUBLISHER)


class LinuxReleasePublicationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.artifact_source = self.root / "artifact-source"
        self.artifact_source.mkdir()
        (self.artifact_source / "ctx").write_bytes(b"candidate\n")
        (self.artifact_source / "ctx.sha256").write_bytes(b"digest\n")
        (self.artifact_source / "ctx").chmod(0o755)
        (self.artifact_source / "ctx.sha256").chmod(0o644)
        self.artifact_leaves = ["ctx", "ctx.sha256"]
        self.symbols_source = self.root / "symbols-source"
        self.symbols_source.mkdir()
        (self.symbols_source / "manifest.json").write_bytes(b"{}\n")
        (self.symbols_source / "symbols.tar.gz").write_bytes(b"symbols\n")
        for path in self.symbols_source.iterdir():
            path.chmod(0o600)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    @staticmethod
    def assert_sentinel(path: Path) -> None:
        if path.read_bytes() != b"sentinel\n":
            raise AssertionError(f"external sentinel changed: {path}")

    def preflight(
        self,
        output: str,
        symbols: str | None,
    ) -> tuple[Path, Path]:
        return PUBLISHER.preflight_destinations(
            self.repo,
            output,
            symbols,
            self.artifact_leaves,
        )

    def publish(self, output: Path, symbols: Path, before_commit=None) -> None:
        PUBLISHER.publish(
            self.artifact_source,
            output,
            self.symbols_source,
            symbols,
            self.artifact_leaves,
            before_commit=before_commit,
        )

    def test_relative_output_resolves_before_default_symbols(self) -> None:
        output, symbols = self.preflight("target/public-cli-artifacts", None)
        self.assertEqual(output, self.repo / "target/public-cli-artifacts")
        self.assertEqual(
            symbols,
            self.repo / "target/public-cli-artifacts.private-debug-symbols",
        )
        self.assertTrue(output.is_dir())
        self.assertTrue(symbols.parent.is_dir())
        self.assertFalse(symbols.exists())

    def test_explicit_external_absolute_destinations_are_preserved(self) -> None:
        output_argument = self.root / "external/public"
        symbols_argument = self.root / "private/symbols"
        output, symbols = self.preflight(
            str(output_argument),
            str(symbols_argument),
        )
        self.assertEqual(output, output_argument)
        self.assertEqual(symbols, symbols_argument)

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
        self.assertEqual(sorted(path.name for path in external.iterdir()), ["sentinel"])
        self.assertFalse(output.exists())

    def test_final_artifact_link_is_rejected_without_following_it(self) -> None:
        output = self.root / "public"
        output.mkdir()
        external_file = self.root / "artifact-sentinel"
        external_file.write_bytes(b"sentinel\n")
        (output / "ctx").symlink_to(external_file)
        symbols = self.root / "private/symbols"

        with self.assertRaisesRegex(PUBLISHER.PublicationError, "already exists"):
            self.preflight(str(output), str(symbols))
        self.assert_sentinel(external_file)
        self.assertFalse(symbols.parent.exists())

    def test_final_symbol_link_is_rejected_without_following_it(self) -> None:
        output = self.root / "public"
        symbols_parent = self.root / "private"
        symbols_parent.mkdir()
        external_symbols = self.root / "external-symbol-directory"
        external_symbols.mkdir()
        symbols = symbols_parent / "symbols"
        symbols.symlink_to(external_symbols, target_is_directory=True)

        with self.assertRaisesRegex(PUBLISHER.PublicationError, "already exists"):
            self.preflight(str(output), str(symbols))
        self.assertEqual(list(external_symbols.iterdir()), [])
        self.assertFalse(output.exists())

    def test_preexisting_artifact_destination_is_never_replaced(self) -> None:
        output, symbols = self.preflight(
            str(self.root / "public"),
            str(self.root / "private/symbols"),
        )
        destination = output / "ctx"
        destination.write_bytes(b"sentinel\n")
        with self.assertRaisesRegex(PUBLISHER.PublicationError, "already exists"):
            self.publish(output, symbols)
        self.assert_sentinel(destination)
        self.assertFalse(symbols.exists())

    def test_preexisting_symbol_destination_is_never_replaced(self) -> None:
        output, symbols = self.preflight(
            str(self.root / "public"),
            str(self.root / "private/symbols"),
        )
        symbols.mkdir()
        sentinel = symbols / "sentinel"
        sentinel.write_bytes(b"sentinel\n")
        with self.assertRaisesRegex(PUBLISHER.PublicationError, "already exists"):
            self.publish(output, symbols)
        self.assert_sentinel(sentinel)
        self.assertEqual(list(output.iterdir()), [])

    def test_publication_preserves_bytes_and_modes(self) -> None:
        output, symbols = self.preflight(
            str(self.root / "public"),
            str(self.root / "private/symbols"),
        )
        self.publish(output, symbols)
        self.assertEqual((output / "ctx").read_bytes(), b"candidate\n")
        self.assertEqual((output / "ctx.sha256").read_bytes(), b"digest\n")
        self.assertEqual((output / "ctx").stat().st_mode & 0o777, 0o755)
        self.assertEqual((output / "ctx.sha256").stat().st_mode & 0o777, 0o644)
        self.assertEqual((symbols / "manifest.json").read_bytes(), b"{}\n")
        self.assertEqual((symbols / "symbols.tar.gz").read_bytes(), b"symbols\n")
        self.assertEqual(symbols.stat().st_mode & 0o777, 0o700)
        self.assertEqual((symbols / "manifest.json").stat().st_mode & 0o777, 0o600)

    def test_concurrent_parent_substitution_stays_on_verified_descriptors(self) -> None:
        output, symbols = self.preflight(
            str(self.root / "public/candidate"),
            str(self.root / "private/symbols"),
        )
        verified_output = self.root / "verified-output"
        verified_symbols_parent = self.root / "verified-symbols-parent"
        external_output = self.root / "external-output"
        external_symbols = self.root / "external-symbols"
        external_output.mkdir()
        external_symbols.mkdir()
        output_sentinel = external_output / "sentinel"
        symbols_sentinel = external_symbols / "sentinel"
        output_sentinel.write_bytes(b"sentinel\n")
        symbols_sentinel.write_bytes(b"sentinel\n")

        def substitute() -> None:
            os.rename(output, verified_output)
            output.symlink_to(external_output, target_is_directory=True)
            os.rename(symbols.parent, verified_symbols_parent)
            symbols.parent.symlink_to(external_symbols, target_is_directory=True)

        self.publish(output, symbols, before_commit=substitute)
        self.assertEqual((verified_output / "ctx").read_bytes(), b"candidate\n")
        self.assertEqual(
            (verified_symbols_parent / symbols.name / "symbols.tar.gz").read_bytes(),
            b"symbols\n",
        )
        self.assert_sentinel(output_sentinel)
        self.assert_sentinel(symbols_sentinel)
        self.assertEqual(sorted(path.name for path in external_output.iterdir()), ["sentinel"])
        self.assertEqual(sorted(path.name for path in external_symbols.iterdir()), ["sentinel"])

    def test_concurrent_final_link_substitution_fails_before_publication(self) -> None:
        output, symbols = self.preflight(
            str(self.root / "public"),
            str(self.root / "private/symbols"),
        )
        external_file = self.root / "artifact-sentinel"
        external_file.write_bytes(b"sentinel\n")
        external_symbols = self.root / "external-symbols"
        external_symbols.mkdir()
        symbols_sentinel = external_symbols / "sentinel"
        symbols_sentinel.write_bytes(b"sentinel\n")

        def substitute() -> None:
            (output / "ctx").symlink_to(external_file)
            symbols.symlink_to(external_symbols, target_is_directory=True)

        with self.assertRaisesRegex(PUBLISHER.PublicationError, "already exists"):
            self.publish(output, symbols, before_commit=substitute)
        self.assert_sentinel(external_file)
        self.assert_sentinel(symbols_sentinel)
        self.assertFalse((output / "ctx.sha256").exists())
        self.assertEqual(
            sorted(path.name for path in output.iterdir()),
            ["ctx"],
        )
        self.assertEqual(
            sorted(path.name for path in symbols.parent.iterdir()),
            ["symbols"],
        )

    def test_task_cleanup_removes_only_the_expected_directory_identity(self) -> None:
        work_root = self.root / "work"
        work_root.mkdir()
        task_root = work_root / "task"
        task_root.mkdir()
        (task_root / "nested").mkdir()
        (task_root / "nested/file").write_bytes(b"staged\n")
        identity = task_root.stat()
        PUBLISHER.cleanup_task_root(
            work_root,
            task_root,
            identity.st_dev,
            identity.st_ino,
        )
        self.assertFalse(task_root.exists())

    def test_concurrent_task_parent_substitution_cannot_redirect_cleanup(self) -> None:
        work_root = self.root / "work"
        work_root.mkdir()
        task_root = work_root / "task"
        task_root.mkdir()
        (task_root / "staged").write_bytes(b"staged\n")
        identity = task_root.stat()
        verified_work_root = self.root / "verified-work"
        external = self.root / "external-work"
        external.mkdir()
        external_task = external / task_root.name
        external_task.mkdir()
        sentinel = external_task / "sentinel"
        sentinel.write_bytes(b"sentinel\n")

        def substitute() -> None:
            os.rename(work_root, verified_work_root)
            work_root.symlink_to(external, target_is_directory=True)

        PUBLISHER.cleanup_task_root(
            work_root,
            task_root,
            identity.st_dev,
            identity.st_ino,
            before_remove=substitute,
        )
        self.assertFalse((verified_work_root / task_root.name).exists())
        self.assert_sentinel(sentinel)


if __name__ == "__main__":
    unittest.main()
