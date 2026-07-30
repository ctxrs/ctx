#!/usr/bin/env python3
"""Create and bind private detached debug symbols for one release binary."""

from __future__ import annotations

import argparse
import gzip
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
from pathlib import Path


SCHEMA_VERSION = 1
KIND = "ctx-detached-debug-symbols"
PLATFORMS = {
    "freebsd-x64": "elf",
    "linux-arm64": "elf",
    "linux-x64": "elf",
    "macos-arm64": "macho",
    "macos-x64": "macho",
    "windows-x64": "pe",
}
MAX_ARCHIVE_BYTES = 2 * 1024 * 1024 * 1024
MAX_ARCHIVE_MEMBERS = 64
MAX_UNCOMPRESSED_BYTES = 4 * 1024 * 1024 * 1024


class SymbolError(RuntimeError):
    pass


def fail(message: str) -> None:
    raise SymbolError(message)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def canonical_bytes(value: object) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
        + "\n"
    ).encode("ascii")


def regular_file(path: Path, label: str) -> Path:
    try:
        metadata = path.lstat()
    except OSError as error:
        fail(f"{label} is unavailable: {error}")
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        fail(f"{label} must be a regular non-symlink file")
    return path.resolve(strict=True)


def require_private_mode(path: Path, label: str) -> None:
    if os.name != "nt" and path.stat().st_mode & 0o077:
        fail(f"{label} must not be accessible by group or other users")


def empty_private_directory(path: Path) -> Path:
    if path.exists() or path.is_symlink():
        fail("symbol output directory must not already exist")
    path.mkdir(mode=0o700, parents=False)
    resolved = path.resolve(strict=True)
    require_private_mode(resolved, "symbol output directory")
    return resolved


def command(name: str) -> str:
    selected = shutil.which(name)
    if selected is None:
        fail(f"required symbol tool is unavailable: {name}")
    return selected


def run(arguments: list[str], *, output: bool = False) -> str:
    try:
        result = subprocess.run(
            arguments,
            check=True,
            stdout=subprocess.PIPE if output else subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            timeout=300,
        )
    except (OSError, subprocess.SubprocessError) as error:
        detail = getattr(error, "stderr", "") or str(error)
        fail(f"symbol tool failed: {arguments[0]}: {detail.strip()}")
    return result.stdout if output else ""


def elf_build_id(path: Path) -> str:
    output = run([command("readelf"), "-nW", str(path)], output=True)
    matches = re.findall(r"Build ID:\s*([0-9a-fA-F]+)", output)
    if len(matches) != 1 or len(matches[0]) < 16:
        fail("ELF artifact has no unique GNU build ID")
    return matches[0].lower()


def macho_uuid(path: Path) -> str:
    output = run([command("dwarfdump"), "--uuid", str(path)], output=True)
    matches = re.findall(r"UUID:\s*([0-9A-Fa-f-]{36})", output)
    if len(matches) != 1:
        fail("Mach-O artifact has no unique UUID")
    return matches[0].lower()


def elf_has_debug_material(path: Path) -> bool:
    output = run([command("readelf"), "-SW", str(path)], output=True)
    section_names = re.findall(r"\[\s*\d+\]\s+(\S+)", output)
    return any(
        name == ".symtab"
        or name.startswith(".zdebug_")
        or (name.startswith(".debug_") and name != ".debug_gdb_scripts")
        for name in section_names
    )


def deterministic_archive(source_root: Path, output: Path) -> list[dict[str, object]]:
    entries: list[dict[str, object]] = []
    paths = sorted(item for item in source_root.rglob("*") if item.is_file())
    if not paths:
        fail("detached symbol set is empty")
    with output.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0, compresslevel=9) as gz:
            with tarfile.open(fileobj=gz, mode="w", format=tarfile.PAX_FORMAT) as archive:
                for path in paths:
                    regular_file(path, "symbol archive member")
                    relative = path.relative_to(source_root).as_posix()
                    info = archive.gettarinfo(str(path), arcname=relative)
                    info.uid = 0
                    info.gid = 0
                    info.uname = ""
                    info.gname = ""
                    info.mtime = 0
                    info.mode = 0o600
                    with path.open("rb") as source:
                        archive.addfile(info, source)
                    entries.append(
                        {
                            "path": relative,
                            "sha256": sha256_file(path),
                            "size": path.stat().st_size,
                        }
                    )
    os.chmod(output, 0o600)
    return entries


def verify_archive(archive: Path, expected_entries: object) -> None:
    if not isinstance(expected_entries, list) or not expected_entries:
        fail("symbol archive entry inventory is invalid")
    if len(expected_entries) > MAX_ARCHIVE_MEMBERS:
        fail("symbol archive contains too many members")
    normalized: list[dict[str, object]] = []
    for entry in expected_entries:
        if (
            not isinstance(entry, dict)
            or set(entry) != {"path", "sha256", "size"}
            or not isinstance(entry.get("path"), str)
            or not isinstance(entry.get("sha256"), str)
            or not isinstance(entry.get("size"), int)
        ):
            fail("symbol archive entry inventory is invalid")
        name = str(entry["path"])
        parts = Path(name).parts
        if (
            not name
            or len(name.encode("utf-8")) > 512
            or name.startswith("/")
            or "\\" in name
            or any(part in {"", ".", ".."} for part in parts)
            or re.fullmatch(r"[0-9a-f]{64}", str(entry["sha256"])) is None
            or int(entry["size"]) <= 0
        ):
            fail("symbol archive entry inventory is invalid")
        normalized.append(
            {
                "path": name,
                "sha256": str(entry["sha256"]),
                "size": int(entry["size"]),
            }
        )
    if normalized != sorted(normalized, key=lambda entry: str(entry["path"])):
        fail("symbol archive entry inventory is not sorted")
    if len({str(entry["path"]) for entry in normalized}) != len(normalized):
        fail("symbol archive entry inventory contains duplicate paths")
    if sum(int(entry["size"]) for entry in normalized) > MAX_UNCOMPRESSED_BYTES:
        fail("symbol archive expands beyond its byte bound")

    observed: list[dict[str, object]] = []
    try:
        with tarfile.open(archive, mode="r:gz") as bundle:
            members = bundle.getmembers()
            if len(members) != len(normalized):
                fail("symbol archive member count differs from its inventory")
            for member in members:
                if not member.isfile() or member.size <= 0:
                    fail("symbol archive contains a non-regular or empty member")
                source = bundle.extractfile(member)
                if source is None:
                    fail("symbol archive member is unreadable")
                digest = hashlib.sha256()
                size = 0
                while chunk := source.read(1024 * 1024):
                    size += len(chunk)
                    if size > MAX_UNCOMPRESSED_BYTES:
                        fail("symbol archive member exceeds its byte bound")
                    digest.update(chunk)
                observed.append(
                    {
                        "path": member.name,
                        "sha256": digest.hexdigest(),
                        "size": size,
                    }
                )
    except (OSError, tarfile.TarError) as error:
        fail(f"symbol archive is invalid: {error}")
    if observed != normalized:
        fail("symbol archive contents differ from its inventory")


def prepare_elf(artifact: Path, symbol_tree: Path) -> tuple[str, str]:
    identifier = elf_build_id(artifact)
    debug_file = symbol_tree / f"{artifact.name}.debug"
    objcopy = command("objcopy")
    run([objcopy, "--only-keep-debug", str(artifact), str(debug_file)])
    if not elf_has_debug_material(debug_file):
        fail("ELF release intermediate contains no detachable debug information")
    run([objcopy, "--strip-all", str(artifact)])
    run([objcopy, f"--add-gnu-debuglink={debug_file}", str(artifact)])
    if elf_build_id(artifact) != identifier:
        fail("ELF build ID changed while detaching symbols")
    if elf_has_debug_material(artifact):
        fail("ELF shipping artifact still contains debug or static symbol sections")
    os.chmod(debug_file, 0o600)
    return "gnu-build-id", identifier


def prepare_macho(artifact: Path, symbol_tree: Path) -> tuple[str, str]:
    identifier = macho_uuid(artifact)
    dsym = symbol_tree / f"{artifact.name}.dSYM"
    run([command("dsymutil"), str(artifact), "-o", str(dsym)])
    dwarf_files = sorted((dsym / "Contents" / "Resources" / "DWARF").glob("*"))
    if len(dwarf_files) != 1 or dwarf_files[0].stat().st_size == 0:
        fail("dsymutil did not produce one nonempty DWARF image")
    if macho_uuid(dwarf_files[0]) != identifier:
        fail("dSYM UUID differs from the release intermediate")
    run([command("strip"), "-S", "-x", str(artifact)])
    if macho_uuid(artifact) != identifier:
        fail("Mach-O UUID changed while detaching symbols")
    return "mach-o-uuid", identifier


def prepare_pe(
    artifact: Path, symbol_tree: Path, pdb: Path | None
) -> tuple[str, str]:
    if pdb is not None:
        pdb = regular_file(pdb, "PDB")
        if pdb.stat().st_size == 0:
            fail("PDB is empty")
        destination = symbol_tree / pdb.name
        shutil.copyfile(pdb, destination)
        os.chmod(destination, 0o600)
        # The exact shipped PE digest in the final manifest is the primary
        # binding. WinDbg additionally validates the PDB's embedded GUID/age.
        run([command("llvm-strip"), "--strip-all", str(artifact)])
        return "pdb-sha256", sha256_file(destination)

    debug_file = symbol_tree / f"{artifact.name}.debug"
    objcopy = command("objcopy")
    run([objcopy, "--only-keep-debug", str(artifact), str(debug_file)])
    if debug_file.stat().st_size == 0:
        fail("GNU PE release intermediate produced no detached debug file")
    run([objcopy, "--strip-all", str(artifact)])
    run([objcopy, f"--add-gnu-debuglink={debug_file}", str(artifact)])
    os.chmod(debug_file, 0o600)
    return "gnu-pe-debug-sha256", sha256_file(debug_file)


def prepare(args: argparse.Namespace) -> None:
    artifact = regular_file(Path(args.artifact), "release intermediate")
    output = empty_private_directory(Path(args.output_dir))
    symbol_tree = output / "payload"
    symbol_tree.mkdir(mode=0o700)
    before_sha = sha256_file(artifact)
    platform_kind = PLATFORMS[args.platform]
    pdb = Path(args.pdb) if args.pdb else None
    if platform_kind == "elf":
        if pdb is not None:
            fail("--pdb is valid only for windows-x64")
        identifier_type, identifier = prepare_elf(artifact, symbol_tree)
    elif platform_kind == "macho":
        if pdb is not None:
            fail("--pdb is valid only for windows-x64")
        identifier_type, identifier = prepare_macho(artifact, symbol_tree)
    else:
        identifier_type, identifier = prepare_pe(artifact, symbol_tree, pdb)

    archive = output / "symbols.tar.gz"
    entries = deterministic_archive(symbol_tree, archive)
    shutil.rmtree(symbol_tree)
    identity = {
        "schema_version": SCHEMA_VERSION,
        "kind": f"{KIND}-prepared",
        "product": args.product,
        "platform": args.platform,
        "binary_name": artifact.name,
        "unstripped_sha256": before_sha,
        "prepared_binary_sha256": sha256_file(artifact),
        "debug_identifier_type": identifier_type,
        "debug_identifier": identifier,
        "archive_file": archive.name,
        "archive_format": "tar.gz",
        "archive_sha256": sha256_file(archive),
        "archive_size": archive.stat().st_size,
        "entries": entries,
    }
    identity_path = output / "prepared.json"
    identity_path.write_bytes(canonical_bytes(identity))
    os.chmod(identity_path, 0o600)


def load_json(path: Path, label: str) -> tuple[bytes, dict[str, object]]:
    path = regular_file(path, label)
    raw = path.read_bytes()
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"{label} is invalid JSON: {error}")
    if not isinstance(value, dict) or raw != canonical_bytes(value):
        fail(f"{label} is not canonical JSON")
    return raw, value


def validate_commit(value: str, label: str) -> str:
    if not re.fullmatch(r"[0-9a-f]{40}", value) or value == "0" * 40:
        fail(f"{label} must be a nonzero lowercase Git commit")
    return value


def finalize(args: argparse.Namespace) -> None:
    artifact = regular_file(Path(args.artifact), "final release artifact")
    output = Path(args.output_dir).resolve(strict=True)
    _, prepared = load_json(output / "prepared.json", "prepared symbol identity")
    archive = regular_file(output / str(prepared.get("archive_file")), "symbol archive")
    if (
        prepared.get("schema_version") != SCHEMA_VERSION
        or prepared.get("kind") != f"{KIND}-prepared"
        or prepared.get("product") != args.product
        or prepared.get("platform") != args.platform
        or prepared.get("binary_name") != artifact.name
        or prepared.get("archive_sha256") != sha256_file(archive)
        or prepared.get("archive_size") != archive.stat().st_size
        or archive.stat().st_size <= 0
        or archive.stat().st_size > MAX_ARCHIVE_BYTES
    ):
        fail("prepared symbol identity differs from finalization inputs")
    require_private_mode(output, "symbol output directory")
    require_private_mode(archive, "symbol archive")
    verify_archive(archive, prepared.get("entries"))

    identifier_type = str(prepared["debug_identifier_type"])
    identifier = str(prepared["debug_identifier"])
    platform_kind = PLATFORMS[args.platform]
    if platform_kind == "elf" and elf_build_id(artifact) != identifier:
        fail("final ELF build ID differs from detached symbols")
    if platform_kind == "macho" and macho_uuid(artifact) != identifier:
        fail("final Mach-O UUID differs from detached symbols")

    source_commit = validate_commit(args.source_commit, "source commit")
    manifest: dict[str, object] = {
        "schema_version": SCHEMA_VERSION,
        "kind": KIND,
        "product": args.product,
        "platform": args.platform,
        "source_commit": source_commit,
        "binary_name": artifact.name,
        "binary_sha256": sha256_file(artifact),
        "binary_size": artifact.stat().st_size,
        "debug_identifier_type": identifier_type,
        "debug_identifier": identifier,
        "archive_file": archive.name,
        "archive_format": prepared["archive_format"],
        "archive_sha256": prepared["archive_sha256"],
        "archive_size": prepared["archive_size"],
        "entries": prepared["entries"],
    }
    if args.public_source_commit:
        manifest["public_source_commit"] = validate_commit(
            args.public_source_commit, "public source commit"
        )
    if args.private_source_commit:
        manifest["private_source_commit"] = validate_commit(
            args.private_source_commit, "private source commit"
        )
    manifest_path = output / "manifest.json"
    manifest_path.write_bytes(canonical_bytes(manifest))
    os.chmod(manifest_path, 0o600)
    (output / "prepared.json").unlink()


def verify_prepared(args: argparse.Namespace) -> None:
    artifact = regular_file(Path(args.artifact), "prepared release artifact")
    output = Path(args.output_dir).resolve(strict=True)
    _, prepared = load_json(output / "prepared.json", "prepared symbol identity")
    archive = regular_file(output / str(prepared.get("archive_file")), "symbol archive")
    if (
        prepared.get("schema_version") != SCHEMA_VERSION
        or prepared.get("kind") != f"{KIND}-prepared"
        or prepared.get("product") != args.product
        or prepared.get("platform") != args.platform
        or prepared.get("binary_name") != artifact.name
        or prepared.get("prepared_binary_sha256") != sha256_file(artifact)
        or prepared.get("archive_sha256") != sha256_file(archive)
        or prepared.get("archive_size") != archive.stat().st_size
    ):
        fail("prepared symbol identity does not match its artifact and archive")
    require_private_mode(output, "symbol output directory")
    require_private_mode(archive, "symbol archive")
    verify_archive(archive, prepared.get("entries"))
    identifier = str(prepared.get("debug_identifier", ""))
    if PLATFORMS[args.platform] == "elf" and elf_build_id(artifact) != identifier:
        fail("prepared ELF build ID differs from detached symbols")
    if PLATFORMS[args.platform] == "macho" and macho_uuid(artifact) != identifier:
        fail("prepared Mach-O UUID differs from detached symbols")


def verify(args: argparse.Namespace) -> None:
    artifact = regular_file(Path(args.artifact), "final release artifact")
    output = Path(args.output_dir).resolve(strict=True)
    _, manifest = load_json(output / "manifest.json", "symbol manifest")
    archive = regular_file(output / str(manifest.get("archive_file")), "symbol archive")
    if (
        manifest.get("schema_version") != SCHEMA_VERSION
        or manifest.get("kind") != KIND
        or manifest.get("product") != args.product
        or manifest.get("platform") != args.platform
        or manifest.get("binary_name") != artifact.name
        or manifest.get("binary_sha256") != sha256_file(artifact)
        or manifest.get("binary_size") != artifact.stat().st_size
        or manifest.get("archive_sha256") != sha256_file(archive)
        or manifest.get("archive_size") != archive.stat().st_size
    ):
        fail("symbol manifest does not match the final artifact and archive")
    require_private_mode(output, "symbol output directory")
    require_private_mode(archive, "symbol archive")
    verify_archive(archive, manifest.get("entries"))
    identifier = str(manifest.get("debug_identifier", ""))
    if PLATFORMS[args.platform] == "elf" and elf_build_id(artifact) != identifier:
        fail("verified ELF build ID differs from the manifest")
    if PLATFORMS[args.platform] == "elf" and elf_has_debug_material(artifact):
        fail("verified ELF artifact still contains debug or static symbol sections")
    if PLATFORMS[args.platform] == "macho" and macho_uuid(artifact) != identifier:
        fail("verified Mach-O UUID differs from the manifest")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    commands = root.add_subparsers(dest="command", required=True)
    prepare_parser = commands.add_parser("prepare")
    verify_prepared_parser = commands.add_parser("verify-prepared")
    finalize_parser = commands.add_parser("finalize")
    verify_parser = commands.add_parser("verify")
    for selected in (
        prepare_parser,
        verify_prepared_parser,
        finalize_parser,
        verify_parser,
    ):
        selected.add_argument("--artifact", required=True)
        selected.add_argument("--output-dir", required=True)
        selected.add_argument("--platform", choices=sorted(PLATFORMS), required=True)
        selected.add_argument("--product", choices=["ctx", "ctx-pro"], required=True)
    prepare_parser.add_argument("--pdb")
    finalize_parser.add_argument("--source-commit", required=True)
    finalize_parser.add_argument("--public-source-commit")
    finalize_parser.add_argument("--private-source-commit")
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        if args.command == "prepare":
            prepare(args)
        elif args.command == "verify-prepared":
            verify_prepared(args)
        elif args.command == "finalize":
            finalize(args)
        else:
            verify(args)
    except SymbolError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
