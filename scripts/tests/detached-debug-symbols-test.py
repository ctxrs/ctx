#!/usr/bin/env python3
"""Linux contract tests for private detached release symbols."""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
TOOL = ROOT / "scripts/release/detached-debug-symbols.py"
SOURCE_COMMIT = "1" * 40


def elf_sections(path: Path) -> set[str]:
    result = subprocess.run(
        ["readelf", "-SW", path],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=30,
    )
    return set(re.findall(r"\[\s*\d+\]\s+(\S+)", result.stdout))


def forbidden_distribution_sections(path: Path) -> set[str]:
    return {
        name
        for name in elf_sections(path)
        if name == ".symtab"
        or name.startswith(".debug")
        or name.startswith(".zdebug")
    }


def run(*arguments: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(TOOL), *arguments],
        check=check,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=60,
    )


class DetachedDebugSymbolsTest(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = Path(tempfile.mkdtemp(prefix="ctx-symbol-test."))
        self.addCleanup(shutil.rmtree, self.directory)
        source = self.directory / "fixture.c"
        source.write_text(
            """
#include <stdio.h>
__asm__(
    ".pushsection .debug_gdb_scripts,\\"MS\\",@progbits,1\\n"
    ".asciz \\"gdb_load_rust_pretty_printers.py\\"\\n"
    ".popsection\\n"
);
static int answer(void) { return 42; }
int main(void) { printf("%d\\n", answer()); return 0; }
""",
            encoding="utf-8",
        )
        self.artifact = self.directory / "ctx"
        subprocess.run(
            ["cc", "-g", "-O1", "-Wl,--build-id=sha1", "-o", self.artifact, source],
            check=True,
            timeout=60,
        )
        self.before_size = self.artifact.stat().st_size
        self.assertIn(".debug_gdb_scripts", elf_sections(self.artifact))
        self.output = self.directory / "private-symbols"

    def prepare_and_finalize(self) -> dict[str, object]:
        run(
            "prepare",
            "--artifact",
            str(self.artifact),
            "--output-dir",
            str(self.output),
            "--platform",
            "linux-x64",
            "--product",
            "ctx",
        )
        run(
            "verify-prepared",
            "--artifact",
            str(self.artifact),
            "--output-dir",
            str(self.output),
            "--platform",
            "linux-x64",
            "--product",
            "ctx",
        )
        run(
            "finalize",
            "--artifact",
            str(self.artifact),
            "--output-dir",
            str(self.output),
            "--platform",
            "linux-x64",
            "--product",
            "ctx",
            "--source-commit",
            SOURCE_COMMIT,
        )
        run(
            "verify",
            "--artifact",
            str(self.artifact),
            "--output-dir",
            str(self.output),
            "--platform",
            "linux-x64",
            "--product",
            "ctx",
        )
        return json.loads((self.output / "manifest.json").read_bytes())

    def test_extracts_strips_binds_and_revalidates(self) -> None:
        manifest = self.prepare_and_finalize()
        archive = self.output / "symbols.tar.gz"
        self.assertLess(self.artifact.stat().st_size, self.before_size)
        self.assertEqual(manifest["source_commit"], SOURCE_COMMIT)
        self.assertEqual(
            manifest["binary_sha256"],
            hashlib.sha256(self.artifact.read_bytes()).hexdigest(),
        )
        self.assertEqual(
            manifest["archive_sha256"],
            hashlib.sha256(archive.read_bytes()).hexdigest(),
        )
        self.assertFalse((self.output / "prepared.json").exists())
        self.assertEqual(stat.S_IMODE(self.output.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(archive.stat().st_mode), 0o600)
        with tarfile.open(archive, mode="r:gz") as bundle:
            members = bundle.getmembers()
            self.assertEqual(len(members), 1)
            self.assertEqual(members[0].name, "ctx.debug")
            self.assertGreater(members[0].size, 0)
            source = bundle.extractfile(members[0])
            self.assertIsNotNone(source)
            archived_debug = self.directory / "archived-ctx.debug"
            archived_debug.write_bytes(source.read())
        self.assertIn(".debug_gdb_scripts", elf_sections(archived_debug))
        self.assertEqual(forbidden_distribution_sections(self.artifact), set())

    def test_tampered_archive_and_binary_fail_closed(self) -> None:
        self.prepare_and_finalize()
        archive = self.output / "symbols.tar.gz"
        with archive.open("ab") as output:
            output.write(b"tampered")
        result = run(
            "verify",
            "--artifact",
            str(self.artifact),
            "--output-dir",
            str(self.output),
            "--platform",
            "linux-x64",
            "--product",
            "ctx",
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not match", result.stderr)

        archive.write_bytes(archive.read_bytes()[:-8])
        with self.artifact.open("ab") as output:
            output.write(b"tampered")
        result = run(
            "verify",
            "--artifact",
            str(self.artifact),
            "--output-dir",
            str(self.output),
            "--platform",
            "linux-x64",
            "--product",
            "ctx",
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)

    def test_rejects_symlink_and_symbol_free_input(self) -> None:
        link = self.directory / "ctx-link"
        link.symlink_to(self.artifact)
        result = run(
            "prepare",
            "--artifact",
            str(link),
            "--output-dir",
            str(self.directory / "link-output"),
            "--platform",
            "linux-x64",
            "--product",
            "ctx",
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("non-symlink", result.stderr)

        stripped = self.directory / "stripped"
        shutil.copyfile(self.artifact, stripped)
        subprocess.run(["strip", "--strip-all", stripped], check=True, timeout=30)
        result = run(
            "prepare",
            "--artifact",
            str(stripped),
            "--output-dir",
            str(self.directory / "stripped-output"),
            "--platform",
            "linux-x64",
            "--product",
            "ctx",
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no detachable debug information", result.stderr)


if __name__ == "__main__":
    unittest.main()
