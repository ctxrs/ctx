#!/usr/bin/env python3
"""Create and bind private detached debug symbols for one release binary."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import mmap
import os
import re
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import uuid
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
PINNED_RUSTC_VERSION = "1.97.1"
PINNED_RUSTC_COMMIT = "8bab26f4f68e0e26f0bb7960be334d5b520ea452"
RUST_HOST_TRIPLES = {
    "freebsd-x64": "x86_64-unknown-freebsd",
    "linux-arm64": "aarch64-unknown-linux-gnu",
    "linux-x64": "x86_64-unknown-linux-gnu",
    "macos-arm64": "aarch64-apple-darwin",
    "macos-x64": "x86_64-apple-darwin",
    "windows-x64": "x86_64-pc-windows-msvc",
}
LINUX_CONSTRUCTION_RUNFILES = {
    "linux-arm64": Path(
        "/build/bazel-links/bin/ctx_release_linux_arm64.runfiles"
    ),
    "linux-x64": Path("/build/bazel-links/bin/ctx_release_linux_x64.runfiles"),
}
MAX_ARCHIVE_BYTES = 2 * 1024 * 1024 * 1024
MAX_ARCHIVE_MEMBERS = 64
MAX_UNCOMPRESSED_BYTES = 4 * 1024 * 1024 * 1024
MACOS_XCODE_SELECT = Path("/usr/bin/xcode-select")
MACOS_XCRUN = Path("/usr/bin/xcrun")
SHT_NOTE = 7


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


def executable_tool(path: Path, label: str) -> Path:
    try:
        resolved = path.resolve(strict=True)
        metadata = resolved.stat()
    except OSError as error:
        fail(f"{label} is unavailable: {error}")
    if not stat.S_ISREG(metadata.st_mode) or not os.access(resolved, os.X_OK):
        fail(f"{label} must be an executable regular file")
    return resolved


def declared_runfile(logical: str, platform: str) -> Path | None:
    workspace = os.environ.get("TEST_WORKSPACE", "_main")
    logical_names = [f"{workspace}/{logical}"]
    if workspace != "_main":
        logical_names.append(f"_main/{logical}")

    roots: list[Path] = []
    if runfiles_dir := os.environ.get("RUNFILES_DIR"):
        roots.append(Path(runfiles_dir))
    construction_root = LINUX_CONSTRUCTION_RUNFILES.get(platform)
    if construction_root is not None:
        roots.append(construction_root)
    for root in roots:
        for name in logical_names:
            candidate = root / name
            if candidate.exists():
                return candidate

    if manifest_name := os.environ.get("RUNFILES_MANIFEST_FILE"):
        manifest = Path(manifest_name)
        try:
            lines = manifest.read_text(encoding="utf-8").splitlines()
        except OSError as error:
            fail(f"declared release runfiles manifest is unavailable: {error}")
        wanted = set(logical_names)
        for line in lines:
            name, separator, physical = line.partition(" ")
            if separator and name in wanted and physical:
                return Path(physical)
    return None


def tool_environment(extra: dict[str, str] | None = None) -> dict[str, str]:
    environment = {"PATH": "/usr/bin:/bin"}
    for name in ("SystemRoot", "SYSTEMROOT", "WINDIR"):
        if value := os.environ.get(name):
            environment[name] = value
    if extra:
        environment.update(extra)
    return environment


def run(
    arguments: list[str],
    *,
    output: bool = False,
    environment: dict[str, str] | None = None,
) -> str:
    try:
        result = subprocess.run(
            arguments,
            check=True,
            env=environment or tool_environment(),
            stdout=subprocess.PIPE if output else subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            timeout=300,
        )
    except (OSError, subprocess.SubprocessError) as error:
        detail = getattr(error, "stderr", "") or str(error)
        fail(f"symbol tool failed: {arguments[0]}: {detail.strip()}")
    return result.stdout if output else ""


def file_identity(path: Path, name: str) -> dict[str, object]:
    return {
        "name": name,
        "sha256": sha256_file(path),
        "size": path.stat().st_size,
    }


def declared_rust_objcopy(
    platform: str,
) -> tuple[dict[str, Path], dict[str, object]]:
    rustc_logical = f"ctx_release_routes/{platform}/rustc"
    rustc_path = declared_runfile(rustc_logical, platform)
    if rustc_path is None:
        fail(
            "declared Bazel Rust toolchain runfile is unavailable: "
            f"{rustc_logical}"
        )
    rustc = executable_tool(rustc_path, "declared Bazel rustc")
    version = run([str(rustc), "--version", "--verbose"], output=True)
    expected_host = RUST_HOST_TRIPLES[platform]
    required_version_lines = {
        f"commit-hash: {PINNED_RUSTC_COMMIT}",
        f"host: {expected_host}",
        f"release: {PINNED_RUSTC_VERSION}",
    }
    if not required_version_lines.issubset(set(version.splitlines())):
        fail("declared Bazel rustc does not match the pinned release toolchain")

    sysroot_output = run([str(rustc), "--print", "sysroot"], output=True)
    sysroot_lines = [line for line in sysroot_output.splitlines() if line]
    if len(sysroot_lines) != 1 or not Path(sysroot_lines[0]).is_absolute():
        fail("declared Bazel rustc returned an invalid sysroot")
    suffix = ".exe" if platform == "windows-x64" else ""
    expected = (
        Path(sysroot_lines[0])
        / "lib"
        / "rustlib"
        / expected_host
        / "bin"
        / f"rust-objcopy{suffix}"
    )
    logical = f"ctx_release_routes/{platform}/rust-objcopy"
    selected_path = declared_runfile(logical, platform)
    if selected_path is None:
        fail(f"declared Bazel Rust tool runfile is unavailable: {logical}")
    selected = executable_tool(selected_path, "declared Bazel rust-objcopy")
    expected = executable_tool(expected, "pinned Rust rust-objcopy")
    if selected != expected:
        fail("declared Bazel rust-objcopy is outside the pinned Rust sysroot")
    tool_version = run([str(selected), "--version"], output=True).strip()
    if (
        f"rust-{PINNED_RUSTC_VERSION}-stable" not in tool_version
        and f"rust {PINNED_RUSTC_VERSION}" not in tool_version
    ):
        fail("declared Bazel rust-objcopy does not match the pinned Rust release")
    identity = {
        "authority": "declared-bazel-rust-toolchain",
        "parser": "builtin-bounded-format-parser-v1",
        "rustc": {
            "commit_hash": PINNED_RUSTC_COMMIT,
            "host": expected_host,
            "release": PINNED_RUSTC_VERSION,
            "sha256": sha256_file(rustc),
        },
        "tools": [
            {
                **file_identity(selected, "rust-objcopy"),
                "version": tool_version,
            }
        ],
    }
    return {"rust-objcopy": selected}, identity


def trusted_platform_file(path: Path, label: str) -> Path:
    selected = executable_tool(path, label)
    metadata = selected.stat()
    if metadata.st_uid != 0 or metadata.st_mode & 0o022:
        fail(f"{label} must be root-owned and not group/other writable")
    return selected


def xcode_developer_directory(xcode_select: Path) -> Path:
    output = run([str(xcode_select), "--print-path"], output=True)
    lines = [line for line in output.splitlines() if line]
    if len(lines) != 1 or not Path(lines[0]).is_absolute():
        fail("xcode-select returned an invalid developer directory")
    try:
        selected = Path(lines[0]).resolve(strict=True)
    except OSError as error:
        fail(f"selected Xcode developer directory is unavailable: {error}")
    parts = selected.parts
    application = (
        len(parts) == 5
        and parts[1] == "Applications"
        and parts[2].endswith(".app")
        and parts[3:] == ("Contents", "Developer")
    )
    command_line_tools = selected == Path("/Library/Developer/CommandLineTools")
    if not application and not command_line_tools:
        fail("xcode-select chose an unsupported developer directory")
    boundary = Path("/").joinpath(*parts[1:3]) if application else selected
    current = selected
    while True:
        metadata = current.stat()
        if metadata.st_uid != 0 or metadata.st_mode & 0o022:
            fail("selected Xcode authority is not root-owned and immutable")
        if current == boundary:
            break
        current = current.parent
    return selected


def xcode_tools() -> tuple[dict[str, Path], dict[str, object]]:
    xcode_select = trusted_platform_file(MACOS_XCODE_SELECT, "platform xcode-select")
    xcrun = trusted_platform_file(MACOS_XCRUN, "platform xcrun")
    developer_dir = xcode_developer_directory(xcode_select)
    environment = tool_environment({"DEVELOPER_DIR": str(developer_dir)})
    tools: dict[str, Path] = {}
    identities: list[dict[str, object]] = []
    for name in ("dsymutil", "strip"):
        output = run(
            [str(xcrun), "--find", name],
            output=True,
            environment=environment,
        )
        lines = [line for line in output.splitlines() if line]
        if len(lines) != 1 or not Path(lines[0]).is_absolute():
            fail(f"xcrun returned an invalid {name} path")
        selected = trusted_platform_file(Path(lines[0]), f"Xcode {name}")
        try:
            selected.relative_to(developer_dir)
        except ValueError:
            fail(f"Xcode {name} is outside the selected developer directory")
        tools[name] = selected
        identities.append(file_identity(selected, name))
    identity = {
        "authority": "xcode-selected-developer-tools",
        "developer_dir": str(developer_dir),
        "parser": "builtin-bounded-format-parser-v1",
        "selector": [
            file_identity(xcode_select, "xcode-select"),
            file_identity(xcrun, "xcrun"),
        ],
        "tools": identities,
    }
    return tools, identity


def release_tools(platform: str) -> tuple[dict[str, Path], dict[str, object]]:
    if PLATFORMS[platform] == "macho":
        return xcode_tools()
    return declared_rust_objcopy(platform)


def verify_tool_authority(
    platform: str, expected: object
) -> dict[str, Path]:
    tools, observed = release_tools(platform)
    if not isinstance(expected, dict) or observed != expected:
        fail("release symbol tool authority differs from the recorded identity")
    return tools


def checked_region(total: int, offset: int, size: int, label: str) -> None:
    if offset < 0 or size < 0 or offset > total or size > total - offset:
        fail(f"{label} is outside the artifact")


def elf_metadata(path: Path) -> tuple[list[str], list[str]]:
    try:
        with path.open("rb") as source, mmap.mmap(
            source.fileno(), 0, access=mmap.ACCESS_READ
        ) as image:
            if len(image) < 64 or image[:4] != b"\x7fELF":
                fail("artifact is not a supported ELF image")
            elf_class = image[4]
            encoding = image[5]
            if encoding == 1:
                endian = "<"
            elif encoding == 2:
                endian = ">"
            else:
                fail("ELF artifact has an unsupported byte order")
            if elf_class == 1:
                section_offset = struct.unpack_from(endian + "I", image, 32)[0]
                entry_size, section_count, names_index = struct.unpack_from(
                    endian + "HHH", image, 46
                )
                section_format = endian + "IIIIIIIIII"
            elif elf_class == 2:
                section_offset = struct.unpack_from(endian + "Q", image, 40)[0]
                entry_size, section_count, names_index = struct.unpack_from(
                    endian + "HHH", image, 58
                )
                section_format = endian + "IIQQQQIIQQ"
            else:
                fail("ELF artifact has an unsupported word size")
            minimum_entry_size = struct.calcsize(section_format)
            if entry_size < minimum_entry_size or section_offset == 0:
                fail("ELF artifact has an invalid section table")

            def section(index: int) -> tuple[int, ...]:
                offset = section_offset + index * entry_size
                checked_region(
                    len(image), offset, minimum_entry_size, "ELF section header"
                )
                return struct.unpack_from(section_format, image, offset)

            section_zero = section(0)
            if section_count == 0:
                section_count = section_zero[5]
            if names_index == 0xFFFF:
                names_index = section_zero[6]
            if section_count <= 0 or section_count > 65_536:
                fail("ELF artifact has an invalid section count")
            checked_region(
                len(image),
                section_offset,
                section_count * entry_size,
                "ELF section table",
            )
            if names_index <= 0 or names_index >= section_count:
                fail("ELF artifact has an invalid section-name table")

            sections = [section(index) for index in range(section_count)]
            names_section = sections[names_index]
            names_offset, names_size = names_section[4], names_section[5]
            checked_region(
                len(image), names_offset, names_size, "ELF section-name table"
            )
            names_data = image[names_offset : names_offset + names_size]

            names: list[str] = []
            build_ids: list[str] = []
            for header in sections:
                name_offset, section_type = header[0], header[1]
                if name_offset >= len(names_data):
                    fail("ELF section name is outside its string table")
                name_end = names_data.find(b"\0", name_offset)
                if name_end < 0:
                    fail("ELF section name is unterminated")
                try:
                    name = names_data[name_offset:name_end].decode("ascii")
                except UnicodeDecodeError:
                    fail("ELF section name is not ASCII")
                names.append(name)
                if section_type != SHT_NOTE:
                    continue
                note_offset, note_size = header[4], header[5]
                checked_region(len(image), note_offset, note_size, "ELF note section")
                cursor = note_offset
                note_end = note_offset + note_size
                while cursor < note_end:
                    checked_region(note_end, cursor, 12, "ELF note header")
                    name_size, descriptor_size, note_type = struct.unpack_from(
                        endian + "III", image, cursor
                    )
                    cursor += 12
                    checked_region(note_end, cursor, name_size, "ELF note name")
                    owner = image[cursor : cursor + name_size].rstrip(b"\0")
                    cursor = (cursor + name_size + 3) & ~3
                    checked_region(
                        note_end, cursor, descriptor_size, "ELF note descriptor"
                    )
                    descriptor = image[cursor : cursor + descriptor_size]
                    cursor = (cursor + descriptor_size + 3) & ~3
                    if cursor > note_end:
                        fail("ELF note padding is outside its section")
                    if owner == b"GNU" and note_type == 3 and descriptor:
                        build_ids.append(descriptor.hex())
            return names, build_ids
    except (OSError, ValueError) as error:
        fail(f"ELF artifact is unreadable: {error}")


def elf_build_id(path: Path) -> str:
    _, build_ids = elf_metadata(path)
    if len(build_ids) != 1 or len(build_ids[0]) < 16:
        fail("ELF artifact has no unique GNU build ID")
    return build_ids[0]


def elf_section_names(path: Path) -> list[str]:
    names, _ = elf_metadata(path)
    return names


def macho_uuids(image: mmap.mmap, offset: int, size: int) -> list[str]:
    checked_region(len(image), offset, size, "Mach-O image")
    checked_region(offset + size, offset, 8, "Mach-O header")
    magic = image[offset : offset + 4]
    thin = {
        b"\xce\xfa\xed\xfe": ("<", 28),
        b"\xcf\xfa\xed\xfe": ("<", 32),
        b"\xfe\xed\xfa\xce": (">", 28),
        b"\xfe\xed\xfa\xcf": (">", 32),
    }
    if magic in thin:
        endian, header_size = thin[magic]
        checked_region(offset + size, offset, header_size, "Mach-O header")
        command_count, commands_size = struct.unpack_from(
            endian + "II", image, offset + 16
        )
        if command_count > 100_000:
            fail("Mach-O image has too many load commands")
        cursor = offset + header_size
        commands_end = cursor + commands_size
        checked_region(offset + size, cursor, commands_size, "Mach-O load commands")
        identifiers: list[str] = []
        for _unused in range(command_count):
            checked_region(commands_end, cursor, 8, "Mach-O load command")
            command, command_size = struct.unpack_from(endian + "II", image, cursor)
            if command_size < 8:
                fail("Mach-O image has an invalid load command")
            checked_region(commands_end, cursor, command_size, "Mach-O load command")
            if command == 0x1B:
                if command_size != 24:
                    fail("Mach-O image has an invalid UUID command")
                identifiers.append(
                    str(uuid.UUID(bytes=image[cursor + 8 : cursor + 24]))
                )
            cursor += command_size
        if cursor != commands_end:
            fail("Mach-O load command sizes are inconsistent")
        return identifiers

    fat = {
        b"\xca\xfe\xba\xbe": (">", "IIIII"),
        b"\xbe\xba\xfe\xca": ("<", "IIIII"),
        b"\xca\xfe\xba\xbf": (">", "IIQQII"),
        b"\xbf\xba\xfe\xca": ("<", "IIQQII"),
    }
    if magic not in fat:
        fail("artifact is not a supported Mach-O image")
    endian, architecture_format = fat[magic]
    architecture_count = struct.unpack_from(endian + "I", image, offset + 4)[0]
    if architecture_count <= 0 or architecture_count > 64:
        fail("Mach-O universal image has an invalid architecture count")
    architecture_size = struct.calcsize(endian + architecture_format)
    checked_region(
        offset + size,
        offset + 8,
        architecture_count * architecture_size,
        "Mach-O universal architecture table",
    )
    identifiers = []
    for index in range(architecture_count):
        fields = struct.unpack_from(
            endian + architecture_format,
            image,
            offset + 8 + index * architecture_size,
        )
        architecture_offset, architecture_length = fields[2], fields[3]
        identifiers.extend(
            macho_uuids(image, offset + architecture_offset, architecture_length)
        )
    return identifiers


def macho_uuid(path: Path) -> str:
    try:
        with path.open("rb") as source, mmap.mmap(
            source.fileno(), 0, access=mmap.ACCESS_READ
        ) as image:
            identifiers = macho_uuids(image, 0, len(image))
    except (OSError, ValueError) as error:
        fail(f"Mach-O artifact is unreadable: {error}")
    if len(identifiers) != 1:
        fail("Mach-O artifact has no unique UUID")
    return identifiers[0]


def elf_has_detachable_debug_material(path: Path) -> bool:
    return any(
        name == ".symtab"
        or name.startswith(".zdebug_")
        or (name.startswith(".debug_") and name != ".debug_gdb_scripts")
        for name in elf_section_names(path)
    )


def elf_distribution_symbol_sections(path: Path) -> list[str]:
    return [
        name
        for name in elf_section_names(path)
        if name == ".symtab"
        or name.startswith(".debug")
        or name.startswith(".zdebug")
    ]


def deterministic_archive(source_root: Path, output: Path) -> list[dict[str, object]]:
    entries: list[dict[str, object]] = []
    paths = sorted(item for item in source_root.rglob("*") if item.is_file())
    if not paths:
        fail("detached symbol set is empty")
    with output.open("wb") as raw:
        with gzip.GzipFile(
            filename="", mode="wb", fileobj=raw, mtime=0, compresslevel=9
        ) as gz:
            with tarfile.open(
                fileobj=gz, mode="w", format=tarfile.PAX_FORMAT
            ) as archive:
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


def prepare_elf(
    artifact: Path, symbol_tree: Path, tools: dict[str, Path]
) -> tuple[str, str]:
    identifier = elf_build_id(artifact)
    debug_file = symbol_tree / f"{artifact.name}.debug"
    objcopy = tools["rust-objcopy"]
    run([str(objcopy), "--only-keep-debug", str(artifact), str(debug_file)])
    if not elf_has_detachable_debug_material(debug_file):
        fail("ELF release intermediate contains no detachable debug information")
    run([str(objcopy), "--strip-all", str(artifact)])
    remaining_debug_sections = [
        name
        for name in elf_distribution_symbol_sections(artifact)
        if name != ".symtab"
    ]
    if remaining_debug_sections:
        run(
            [
                str(objcopy),
                *(f"--remove-section={name}" for name in remaining_debug_sections),
                str(artifact),
            ]
        )
    run([str(objcopy), f"--add-gnu-debuglink={debug_file}", str(artifact)])
    if elf_build_id(artifact) != identifier:
        fail("ELF build ID changed while detaching symbols")
    if elf_distribution_symbol_sections(artifact):
        fail("ELF shipping artifact still contains debug or static symbol sections")
    os.chmod(debug_file, 0o600)
    return "gnu-build-id", identifier


def prepare_macho(
    artifact: Path, symbol_tree: Path, tools: dict[str, Path]
) -> tuple[str, str]:
    identifier = macho_uuid(artifact)
    dsym = symbol_tree / f"{artifact.name}.dSYM"
    run([str(tools["dsymutil"]), str(artifact), "-o", str(dsym)])
    dwarf_files = sorted((dsym / "Contents" / "Resources" / "DWARF").glob("*"))
    if len(dwarf_files) != 1 or dwarf_files[0].stat().st_size == 0:
        fail("dsymutil did not produce one nonempty DWARF image")
    if macho_uuid(dwarf_files[0]) != identifier:
        fail("dSYM UUID differs from the release intermediate")
    # Rust's Mach-O link can leave an N_OPT `radr://...` entry after the
    # conventional debug/local-symbol strip.  It is an nlist marker rather
    # than a runtime export, and the shipping CLI does not expose an nlist
    # interface.  Remove the nlist/string table as well so the detached dSYM
    # is the only symbol authority carried by the release.
    run([str(tools["strip"]), "-S", "-x", "-N", str(artifact)])
    if macho_uuid(artifact) != identifier:
        fail("Mach-O UUID changed while detaching symbols")
    return "mach-o-uuid", identifier


def prepare_pe(
    artifact: Path,
    symbol_tree: Path,
    pdb: Path | None,
    tools: dict[str, Path],
) -> tuple[str, str]:
    objcopy = tools["rust-objcopy"]
    if pdb is not None:
        pdb = regular_file(pdb, "PDB")
        if pdb.stat().st_size == 0:
            fail("PDB is empty")
        destination = symbol_tree / pdb.name
        shutil.copyfile(pdb, destination)
        os.chmod(destination, 0o600)
        # The exact shipped PE digest in the final manifest is the primary
        # binding. WinDbg additionally validates the PDB's embedded GUID/age.
        run(
            [
                str(objcopy),
                "--strip-all",
                str(artifact),
            ]
        )
        return "pdb-sha256", sha256_file(destination)

    debug_file = symbol_tree / f"{artifact.name}.debug"
    run([str(objcopy), "--only-keep-debug", str(artifact), str(debug_file)])
    if debug_file.stat().st_size == 0:
        fail("GNU PE release intermediate produced no detached debug file")
    run([str(objcopy), "--strip-all", str(artifact)])
    run([str(objcopy), f"--add-gnu-debuglink={debug_file}", str(artifact)])
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
    tools, tool_authority = release_tools(args.platform)
    if platform_kind == "elf":
        if pdb is not None:
            fail("--pdb is valid only for windows-x64")
        identifier_type, identifier = prepare_elf(artifact, symbol_tree, tools)
    elif platform_kind == "macho":
        if pdb is not None:
            fail("--pdb is valid only for windows-x64")
        identifier_type, identifier = prepare_macho(artifact, symbol_tree, tools)
    else:
        identifier_type, identifier = prepare_pe(artifact, symbol_tree, pdb, tools)

    verify_tool_authority(args.platform, tool_authority)

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
        "tool_authority": tool_authority,
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
    verify_tool_authority(args.platform, prepared.get("tool_authority"))
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
        "tool_authority": prepared["tool_authority"],
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
    verify_tool_authority(args.platform, prepared.get("tool_authority"))
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
    verify_tool_authority(args.platform, manifest.get("tool_authority"))
    identifier = str(manifest.get("debug_identifier", ""))
    if PLATFORMS[args.platform] == "elf" and elf_build_id(artifact) != identifier:
        fail("verified ELF build ID differs from the manifest")
    if (
        PLATFORMS[args.platform] == "elf"
        and elf_distribution_symbol_sections(artifact)
    ):
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
