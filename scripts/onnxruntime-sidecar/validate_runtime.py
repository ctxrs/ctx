#!/usr/bin/env python3
"""Validate a staged semantic runtime library and load ONNX Runtime when requested."""

import argparse
import ctypes
import os
import re
import struct
import subprocess
from pathlib import Path


CUDA_EXTERNAL_ELF_DEPENDENCIES = {
    "ld-linux-x86-64.so.2",
    "libc.so.6",
    "libcuda.so.1",
    "libdl.so.2",
    "libgcc_s.so.1",
    "libm.so.6",
    "libnvidia-ml.so.1",
    "libpthread.so.0",
    "librt.so.1",
    "libstdc++.so.6",
}


def validate_elf_dependency_closure(root: Path, libraries: list[str]) -> None:
    bundled = {
        path.name
        for path in root.iterdir()
        if path.is_file() and not path.is_symlink()
    }
    if len(libraries) != len(set(libraries)):
        raise SystemExit("duplicate CUDA dependency-closure input")
    for library in libraries:
        if "/" in library or "\\" in library or library not in bundled:
            raise SystemExit(f"invalid CUDA dependency-closure input: {library}")
        try:
            output = subprocess.run(
                ["readelf", "-d", "--wide", str(root / library)],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
        except (OSError, subprocess.CalledProcessError) as error:
            raise SystemExit(f"could not inspect ELF dependencies for {library}") from error
        needed = set(
            re.findall(r"\(NEEDED\).*Shared library: \[([^\]]+)\]", output)
        )
        unresolved = sorted(
            needed - bundled - CUDA_EXTERNAL_ELF_DEPENDENCIES
        )
        if unresolved:
            raise SystemExit(
                f"CUDA archive has unresolved ELF dependencies for {library}: "
                + ", ".join(unresolved)
            )
    print(
        "CUDA static ELF dependency closure ok; GPU execution-provider "
        "qualification remains deferred"
    )


def freebsd_provenance(
    recipe: str,
    source_sha256: str,
    ports_commit: str,
    deps_sha256: str,
    freebsd_abi: str,
) -> tuple[str, ...]:
    return (
        f"ctx-recipe={recipe}",
        f"ctx-source-sha256={source_sha256}",
        f"ctx-freebsd-ports={ports_commit}",
        f"ctx-deps-sha256={deps_sha256}",
        f"ctx-freebsd-abi={freebsd_abi}",
        f"ctx-freebsd-userland={freebsd_abi}.",
        "ctx-os=FreeBSD-",
        "ctx-compiler=Clang-",
        "ctx-cmake=",
        "build type=Release",
    )


def validate_binary(
    platform: str,
    path: Path,
    version: str,
    max_glibc: str,
    recipe: str,
    source_sha256: str,
    ports_commit: str,
    deps_sha256: str,
    freebsd_abi: str,
    require_version_marker: bool = True,
) -> None:
    binary_platform = (
        "linux-x64"
        if platform == "linux-x64-cuda12"
        else "windows-x64"
        if platform == "windows-x64-windowsml"
        else platform
    )
    data = path.read_bytes()
    if len(data) < 64:
        raise SystemExit(
            f"native runtime library is implausibly small: {len(data)} bytes"
        )
    if require_version_marker and version.encode() not in data:
        raise SystemExit(
            f"native runtime library does not contain version marker {version}"
        )

    if binary_platform.startswith("linux-") or binary_platform == "freebsd-x64":
        if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
            raise SystemExit("native runtime library is not 64-bit little-endian ELF")
        elf_type, machine = struct.unpack_from("<HH", data, 16)
        expected_machine = 183 if binary_platform == "linux-aarch64" else 62
        if elf_type != 3:
            raise SystemExit(
                f"native runtime ELF type is {elf_type}, expected ET_DYN (3)"
            )
        if machine != expected_machine:
            raise SystemExit(
                f"native runtime ELF machine is {machine}, "
                f"expected {expected_machine} for {platform}"
            )
        osabi = data[7]
        if binary_platform == "freebsd-x64":
            if osabi != 9:
                raise SystemExit(
                    f"native runtime ELF OSABI is {osabi}, expected FreeBSD (9)"
                )
            required = freebsd_provenance(
                recipe, source_sha256, ports_commit, deps_sha256, freebsd_abi
            )
            missing = [marker for marker in required if marker.encode() not in data]
            if missing:
                raise SystemExit(
                    "native FreeBSD runtime is missing pinned build provenance: "
                    + ", ".join(missing)
                )
        elif osabi not in (0, 3):
            raise SystemExit(
                f"native runtime ELF OSABI is {osabi}, expected System V or GNU/Linux"
            )
        if binary_platform.startswith("linux-"):
            versions = {
                (int(match.group(1)), int(match.group(2)))
                for match in re.finditer(rb"GLIBC_(\d+)\.(\d+)", data)
            }
            if not versions:
                raise SystemExit("native Linux runtime has no GLIBC symbol versions")
            allowed = tuple(int(part) for part in max_glibc.split("."))
            required = max(versions)
            if required > allowed:
                raise SystemExit(
                    f"native Linux runtime requires GLIBC_{required[0]}.{required[1]}, "
                    f"newer than allowed GLIBC_{allowed[0]}.{allowed[1]}"
                )
    elif binary_platform.startswith("macos-"):
        magic, cpu_type, _cpu_subtype, file_type = struct.unpack_from("<IIII", data, 0)
        expected_cpu = 0x0100000C if binary_platform == "macos-arm64" else 0x01000007
        if magic != 0xFEEDFACF:
            raise SystemExit(
                "native runtime library is not a thin 64-bit little-endian Mach-O"
            )
        if cpu_type != expected_cpu:
            raise SystemExit(
                f"native runtime Mach-O CPU type is 0x{cpu_type:08x}, "
                f"expected 0x{expected_cpu:08x} for {platform}"
            )
        if file_type != 6:
            raise SystemExit(
                f"native runtime Mach-O file type is {file_type}, expected MH_DYLIB (6)"
            )
    elif binary_platform == "windows-x64":
        if data[:2] != b"MZ" or len(data) < 0x40:
            raise SystemExit("native runtime library is not a PE image")
        pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
        if pe_offset + 26 > len(data) or data[pe_offset : pe_offset + 4] != b"PE\0\0":
            raise SystemExit("native runtime library has an invalid PE header")
        machine = struct.unpack_from("<H", data, pe_offset + 4)[0]
        characteristics = struct.unpack_from("<H", data, pe_offset + 22)[0]
        optional_magic = struct.unpack_from("<H", data, pe_offset + 24)[0]
        if machine != 0x8664:
            raise SystemExit(
                f"native runtime PE machine is 0x{machine:04x}, expected AMD64"
            )
        if optional_magic != 0x20B:
            raise SystemExit("native runtime PE image is not PE32+")
        if not characteristics & 0x2000:
            raise SystemExit("native runtime PE image is not marked as a DLL")
    else:
        raise SystemExit(f"unsupported platform: {platform}")


def uname(flag: str) -> str:
    try:
        return subprocess.run(
            ["uname", flag], check=True, capture_output=True, text=True
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return "unknown"


def host_matches(platform: str, system: str, machine: str) -> bool:
    if platform in ("linux-x64", "linux-x64-cuda12"):
        return system == "Linux" and machine in ("x86_64", "amd64")
    if platform == "linux-aarch64":
        return system == "Linux" and machine in ("aarch64", "arm64")
    if platform == "macos-arm64":
        return system == "Darwin" and machine == "arm64"
    if platform == "macos-x64":
        return system == "Darwin" and machine == "x86_64"
    if platform == "freebsd-x64":
        return system == "FreeBSD" and machine in ("x86_64", "amd64")
    if platform in ("windows-x64", "windows-x64-windowsml"):
        return (
            system.startswith(("MINGW", "MSYS", "CYGWIN")) and machine == "x86_64"
        )
    return False


def validate_loaded_runtime(
    platform: str,
    path: Path,
    expected: str,
    api_version: str,
    recipe: str,
    source_sha256: str,
    ports_commit: str,
    deps_sha256: str,
    freebsd_abi: str,
) -> None:
    system = uname("-s")
    machine = uname("-m")
    if not host_matches(platform, system, machine):
        print(
            f"native ONNX Runtime load check skipped on {system}/{machine} for {platform}"
        )
        return

    if os.name == "nt":
        os.add_dll_directory(str(path.resolve().parent))
    runtime = ctypes.CDLL(str(path.resolve()))
    runtime.OrtGetApiBase.argtypes = []
    runtime.OrtGetApiBase.restype = ctypes.c_void_p
    base = runtime.OrtGetApiBase()
    if not base:
        raise SystemExit("OrtGetApiBase returned null")
    entries = ctypes.cast(base, ctypes.POINTER(ctypes.c_void_p))
    callback_type = (
        getattr(ctypes, "WINFUNCTYPE", ctypes.CFUNCTYPE)
        if os.name == "nt"
        else ctypes.CFUNCTYPE
    )
    get_api = callback_type(ctypes.c_void_p, ctypes.c_uint32)(entries[0])
    api = get_api(int(api_version))
    if not api:
        raise SystemExit(f"OrtApiBase::GetApi({api_version}) returned null")
    get_version = callback_type(ctypes.c_char_p)(entries[1])
    actual_bytes = get_version()
    actual = actual_bytes.decode("utf-8") if actual_bytes else ""
    if actual != expected:
        raise SystemExit(
            f"OrtGetVersionString returned {actual!r}, expected {expected!r}"
        )
    if platform == "freebsd-x64":
        api_entries = ctypes.cast(api, ctypes.POINTER(ctypes.c_void_p))
        get_build_info_address = api_entries[254]
        if not get_build_info_address:
            raise SystemExit("OrtApi::GetBuildInfoString is null")
        get_build_info = callback_type(ctypes.c_char_p)(get_build_info_address)
        build_info_bytes = get_build_info()
        build_info = build_info_bytes.decode("utf-8") if build_info_bytes else ""
        required = freebsd_provenance(
            recipe, source_sha256, ports_commit, deps_sha256, freebsd_abi
        )
        missing = [marker for marker in required if marker not in build_info]
        if missing:
            raise SystemExit(
                "OrtGetBuildInfoString is missing pinned FreeBSD provenance: "
                + ", ".join(missing)
            )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--platform", required=True)
    parser.add_argument("--library", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--api-version", required=True)
    parser.add_argument("--max-glibc", required=True)
    parser.add_argument("--freebsd-build-recipe", required=True)
    parser.add_argument("--source-sha256", required=True)
    parser.add_argument("--freebsd-ports-commit", required=True)
    parser.add_argument("--freebsd-deps-sha256", required=True)
    parser.add_argument("--freebsd-abi", required=True)
    parser.add_argument("--skip-version-marker", action="store_true")
    parser.add_argument("--skip-load-check", action="store_true")
    parser.add_argument("--dependency-root", type=Path)
    parser.add_argument("--dependency-library", action="append", default=[])
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    validate_binary(
        args.platform,
        args.library,
        args.version,
        args.max_glibc,
        args.freebsd_build_recipe,
        args.source_sha256,
        args.freebsd_ports_commit,
        args.freebsd_deps_sha256,
        args.freebsd_abi,
        not args.skip_version_marker,
    )
    if bool(args.dependency_root) != bool(args.dependency_library):
        raise SystemExit(
            "--dependency-root and --dependency-library must be provided together"
        )
    if args.dependency_root:
        validate_elf_dependency_closure(
            args.dependency_root, args.dependency_library
        )
    if not args.skip_load_check:
        validate_loaded_runtime(
            args.platform,
            args.library,
            args.version,
            args.api_version,
            args.freebsd_build_recipe,
            args.source_sha256,
            args.freebsd_ports_commit,
            args.freebsd_deps_sha256,
            args.freebsd_abi,
        )


if __name__ == "__main__":
    main()
