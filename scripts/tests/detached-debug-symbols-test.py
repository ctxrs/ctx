#!/usr/bin/env python3
"""Linux contract tests for private detached release symbols."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import re
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
import uuid
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
TOOL = ROOT / "scripts/release/detached-debug-symbols.py"
SOURCE_COMMIT = "1" * 40
if len(sys.argv) < 3:
    raise RuntimeError("test requires the declared Bazel Rust tool runfiles")
PINNED_RUSTC = Path(sys.argv.pop(1)).resolve(strict=True)
PINNED_RUST_OBJCOPY = Path(sys.argv.pop(1)).resolve(strict=True)
SPEC = importlib.util.spec_from_file_location("detached_debug_symbols", TOOL)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load detached symbol tool")
SYMBOL_TOOL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SYMBOL_TOOL)


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


def run(
    *arguments: str,
    check: bool = True,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(TOOL), *arguments],
        check=check,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=60,
        env=env,
    )


class DetachedDebugSymbolsTest(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = Path(tempfile.mkdtemp(prefix="ctx-symbol-test."))
        self.addCleanup(shutil.rmtree, self.directory)
        self.manifest = self.directory / "declared-runfiles-manifest"
        self.manifest.write_text(
            "_main/ctx_release_routes/linux-x64/rustc "
            f"{PINNED_RUSTC}\n"
            "_main/ctx_release_routes/linux-x64/rust-objcopy "
            f"{PINNED_RUST_OBJCOPY}\n",
            encoding="utf-8",
        )
        prior_manifest = os.environ.get("RUNFILES_MANIFEST_FILE")
        prior_runfiles_dir = os.environ.pop("RUNFILES_DIR", None)
        os.environ["RUNFILES_MANIFEST_FILE"] = str(self.manifest)

        def restore_runfiles_environment() -> None:
            if prior_manifest is None:
                os.environ.pop("RUNFILES_MANIFEST_FILE", None)
            else:
                os.environ["RUNFILES_MANIFEST_FILE"] = prior_manifest
            if prior_runfiles_dir is not None:
                os.environ["RUNFILES_DIR"] = prior_runfiles_dir

        self.addCleanup(restore_runfiles_environment)
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

    def prepare_and_finalize(
        self, env: dict[str, str] | None = None
    ) -> dict[str, object]:
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
            env=env,
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
            env=env,
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
            env=env,
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
            env=env,
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
        self.assertEqual(
            manifest["tool_authority"]["authority"],
            "declared-bazel-rust-toolchain",
        )
        self.assertEqual(
            [tool["name"] for tool in manifest["tool_authority"]["tools"]],
            ["rust-objcopy"],
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

    def test_builtin_macho_parser_reads_a_thin_uuid(self) -> None:
        identifier = uuid.UUID("12345678-1234-5678-9abc-def012345678")
        macho = self.directory / "thin-macho"
        macho.write_bytes(
            struct.pack(
                "<IiiIIIII",
                0xFEEDFACF,
                0x01000007,
                3,
                2,
                1,
                24,
                0,
                0,
            )
            + struct.pack("<II", 0x1B, 24)
            + identifier.bytes
        )
        self.assertEqual(SYMBOL_TOOL.macho_uuid(macho), str(identifier))

    def test_macho_prepare_removes_the_nlist_symbol_table(self) -> None:
        artifact = self.directory / "ctx-macos"
        artifact.write_bytes(b"shipping binary")
        symbol_tree = self.directory / "macho-symbols"
        symbol_tree.mkdir()
        dsymutil = self.directory / "dsymutil"
        strip = self.directory / "strip"
        calls: list[list[str]] = []

        def fake_run(arguments: list[str], **_kwargs: object) -> str:
            command = [str(argument) for argument in arguments]
            calls.append(command)
            if command[0] == str(dsymutil):
                output = Path(command[command.index("-o") + 1])
                dwarf = output / "Contents" / "Resources" / "DWARF" / artifact.name
                dwarf.parent.mkdir(parents=True)
                dwarf.write_bytes(b"detached symbols")
            return ""

        identifier = "12345678-1234-5678-9abc-def012345678"
        with (
            mock.patch.object(SYMBOL_TOOL, "run", side_effect=fake_run),
            mock.patch.object(SYMBOL_TOOL, "macho_uuid", return_value=identifier),
        ):
            self.assertEqual(
                SYMBOL_TOOL.prepare_macho(
                    artifact,
                    symbol_tree,
                    {"dsymutil": dsymutil, "strip": strip},
                ),
                ("mach-o-uuid", identifier),
            )

        self.assertIn(
            [str(strip), "-S", "-x", "-N", str(artifact)],
            calls,
        )

    def test_ignores_all_hostile_ambient_symbol_tools(self) -> None:
        hostile_bin = self.directory / "hostile-bin"
        hostile_bin.mkdir()
        marker = self.directory / "hostile-symbol-tool-ran"
        for name in (
            "dsymutil",
            "dwarfdump",
            "llvm-objcopy",
            "llvm-strip",
            "objcopy",
            "readelf",
            "rust-objcopy",
            "strip",
        ):
            executable = hostile_bin / name
            executable.write_text(
                '#!/bin/sh\n: > "$HOSTILE_SYMBOL_TOOL_MARKER"\nexit 99\n',
                encoding="utf-8",
            )
            executable.chmod(0o700)
        environment = os.environ.copy()
        environment["PATH"] = f"{hostile_bin}:{environment['PATH']}"
        environment["HOSTILE_SYMBOL_TOOL_MARKER"] = str(marker)
        self.prepare_and_finalize(env=environment)
        self.assertFalse(marker.exists())

    def test_missing_declared_mutator_fails_without_path_fallback(self) -> None:
        hostile_bin = self.directory / "hostile-bin"
        hostile_bin.mkdir()
        marker = self.directory / "hostile-objcopy-ran"
        for name in ("objcopy", "llvm-objcopy", "rust-objcopy"):
            executable = hostile_bin / name
            executable.write_text(
                '#!/bin/sh\n: > "$HOSTILE_OBJCOPY_MARKER"\nexit 99\n',
                encoding="utf-8",
            )
            executable.chmod(0o700)
        self.manifest.write_text(
            "_main/ctx_release_routes/linux-x64/rustc "
            f"{PINNED_RUSTC}\n",
            encoding="utf-8",
        )
        environment = os.environ.copy()
        environment["PATH"] = f"{hostile_bin}:{environment['PATH']}"
        environment["HOSTILE_OBJCOPY_MARKER"] = str(marker)
        result = run(
            "prepare",
            "--artifact",
            str(self.artifact),
            "--output-dir",
            str(self.output),
            "--platform",
            "linux-x64",
            "--product",
            "ctx",
            env=environment,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("rust-objcopy", result.stderr)
        self.assertFalse(marker.exists())

    def test_tampered_tool_identity_fails_closed(self) -> None:
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
        identity_path = self.output / "prepared.json"
        identity = json.loads(identity_path.read_bytes())
        identity["tool_authority"]["tools"][0]["sha256"] = "0" * 64
        identity_path.write_text(
            json.dumps(identity, sort_keys=True, separators=(",", ":")) + "\n",
            encoding="ascii",
        )
        result = run(
            "verify-prepared",
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
        self.assertIn("tool authority differs", result.stderr)

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
