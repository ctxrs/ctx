#!/usr/bin/env python3

import hashlib
import importlib.util
import io
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path


sys.dont_write_bytecode = True

REPO_ROOT = Path(__file__).resolve().parents[2]
TOOLS = REPO_ROOT / "scripts" / "onnxruntime-sidecar"
VERSION = "1.27.0"
COMMIT = "8f0278c77bf44b0cc83c098c6c722b92a36ac4b5"


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


archive_tool = load_module("archive_tool", TOOLS / "archive_tool.py")
validate_runtime = load_module("validate_runtime", TOOLS / "validate_runtime.py")


def shell_manifest(script: str, *args: str) -> dict[str, str]:
    result = subprocess.run(
        ["bash", str(TOOLS / script), *args],
        check=True,
        capture_output=True,
        text=True,
    )
    return dict(line.split("=", 1) for line in result.stdout.splitlines())


def write_stage(root: Path, library: str) -> tuple[str, ...]:
    files = archive_tool.archive_files(library)
    for name in files:
        path = root.joinpath(*name.split("/"))
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(f"fixture:{name}\n".encode())
    return files


def replace_assignment(path: Path, name: str, value: str) -> None:
    lines = path.read_text().splitlines()
    prefix = f'{name}="'
    matches = [index for index, line in enumerate(lines) if line.startswith(prefix)]
    if len(matches) != 1:
        raise AssertionError(f"expected one {name} assignment in {path}")
    lines[matches[0]] = f'{name}="{value}"'
    path.write_text("\n".join(lines) + "\n")


class ManifestTests(unittest.TestCase):
    def test_all_platform_release_shapes_are_exact(self) -> None:
        expected = {
            "linux-x64": ("ctx-onnxruntime-linux-x64.tar.zst", "tar.zst", "libonnxruntime.so", "official"),
            "linux-aarch64": ("ctx-onnxruntime-linux-aarch64.tar.zst", "tar.zst", "libonnxruntime.so", "official"),
            "macos-arm64": ("ctx-onnxruntime-macos-arm64.tar.zst", "tar.zst", "libonnxruntime.dylib", "official"),
            "macos-x64": ("ctx-onnxruntime-macos-x64.tar.zst", "tar.zst", "libonnxruntime.dylib", "macos-x64-source"),
            "windows-x64": ("ctx-onnxruntime-windows-x64.zip", "zip", "onnxruntime.dll", "official"),
            "freebsd-x64": ("ctx-onnxruntime-freebsd-x64.tar.zst", "tar.zst", "libonnxruntime.so", "freebsd-x64-source"),
        }
        for platform, shape in expected.items():
            with self.subTest(platform=platform):
                manifest = shell_manifest("release_manifest.sh", platform)
                self.assertEqual(manifest["version"], VERSION)
                self.assertEqual(manifest["api_version"], "24")
                self.assertEqual(manifest["commit"], COMMIT)
                self.assertEqual(manifest["max_glibc"], "2.39")
                self.assertEqual(manifest["freebsd_build_recipe"], "ctx-freebsd-source-v1")
                self.assertEqual(manifest["freebsd_abi"], "14")
                self.assertEqual(manifest["source_date_epoch"], "1781827200")
                self.assertEqual(
                    (
                        manifest["asset_name"],
                        manifest["archive_kind"],
                        manifest["library_name"],
                        manifest["stage_kind"],
                    ),
                    shape,
                )

    def test_source_input_manifest_preserves_every_pin(self) -> None:
        manifest = shell_manifest("source_inputs.sh", "linux-x64")
        expected = {
            "source_sha256": "b41d09905a3c2f3a25709d1dcce8ef3942a4c2799d1046f74be7b6bbebc45e6a",
            "license_sha256": "2f07c72751aed99790b8a4869cf2311df85a860b22ded05fa22803587a48922c",
            "notices_sha256": "0e07b95f3a8d6230037707c5c4a2b554d12c4cb67369669ac255635528ffcee2",
            "deps_sha256": "e411468ead299e3386b2e5e9d773e50e1939b5fc0baca599666ca5757eeb3f71",
            "windows_vc_redist_sha256": "cc0ff0eb1dc3f5188ae6300faef32bf5beeba4bdd6e8e445a9184072096b713b",
            "windows_vc_minimum_cab_sha256": "640aa6c516c72444523b8fbe034db46ff4e118ed02705340e3ccb62d426ff040",
            "windows_vc_license_sha256": "8099dc3cf9502c335da829e5c755948a12e3e6de490eb492a99deb673d883d8b",
            "windows_msvcp140_sha256": "0f885b509a685d2bbfa652fed26b5fb31d88fbdab0a978c641d1c7b8aa460aa9",
            "windows_msvcp140_1_sha256": "bfad5aef4c63a669e3c140655cdfdf395b6c979b400a447bd5dcb65ed8826c3d",
            "windows_vcruntime140_sha256": "d5e4d9a3e835fa679450145d6a7d94e36573a509317111904d9b3712c30d9066",
            "windows_vcruntime140_1_sha256": "1f2d41c4aa5db0bc33ebf7b66d72943a817d7ce6cbe880502a9403823633093f",
            "freebsd_ports_commit": "7c1f125705820cd2b776056f2c492ed605f3b5e3",
            "freebsd_spin_pause_patch_sha256": "37f30419946cc3440859d4ce2bccf05b3a8961dd9b3b2dd9f9663b6a235282c1",
            "freebsd_posix_env_patch_sha256": "d730c2fe1341654159f1068beaf224f06cffb5520593718681c96fb47e131033",
            "freebsd_distinfo_sha256": "ef17d849c2707c0db508504f982565238a80af66c33b3261973ec29bc7e72b5e",
            "upstream_sha256": "547e40a48f1fe73e3f812d7c88a948612c23f896b91e4e2ee1e232d7b468246f",
        }
        for key, value in expected.items():
            self.assertEqual(manifest[key], value)

        official = {
            "linux-x64": (
                "onnxruntime-linux-x64-1.27.0.tgz",
                "547e40a48f1fe73e3f812d7c88a948612c23f896b91e4e2ee1e232d7b468246f",
                "onnxruntime-linux-x64-1.27.0",
                "lib/libonnxruntime.so.1.27.0",
            ),
            "linux-aarch64": (
                "onnxruntime-linux-aarch64-1.27.0.tgz",
                "3e4d83ac06924a32a07b6d7f91ce6f852876153fc0bbdf931bf517a140bfbe48",
                "onnxruntime-linux-aarch64-1.27.0",
                "lib/libonnxruntime.so.1.27.0",
            ),
            "macos-arm64": (
                "onnxruntime-osx-arm64-1.27.0.tgz",
                "545e81c58152353acb0d1e8bd6ce4b62f830c0961f5b3acfedc790ffd76e477a",
                "onnxruntime-osx-arm64-1.27.0",
                "lib/libonnxruntime.dylib",
            ),
            "windows-x64": (
                "onnxruntime-win-x64-1.27.0.zip",
                "c5c81710938e68079ff1a192b04897faabe4b43830d48f39f27ecd4e16138bfc",
                "onnxruntime-win-x64-1.27.0",
                "lib/onnxruntime.dll",
            ),
        }
        for platform, expected_source in official.items():
            with self.subTest(platform=platform):
                source = shell_manifest("source_inputs.sh", platform)
                self.assertEqual(
                    (
                        source["upstream_asset"],
                        source["upstream_sha256"],
                        source["upstream_root"],
                        source["upstream_library"],
                    ),
                    expected_source,
                )

    def test_coordinator_alone_owns_signing_and_publication(self) -> None:
        coordinator = (REPO_ROOT / "scripts" / "build-onnxruntime-sidecar.sh").read_text()
        self.assertLessEqual(len(coordinator.splitlines()), 220)
        self.assertIn("run-macos-release-signing.sh", coordinator)
        self.assertIn("macos-release-signing-evidence.py", coordinator)
        self.assertIn('mv "${temporary_output}"', coordinator)
        for path in TOOLS.iterdir():
            if not path.is_file():
                continue
            text = path.read_text()
            self.assertNotIn("run-macos-release-signing.sh", text, path.name)
            self.assertNotIn("macos-release-signing-evidence.py", text, path.name)
            self.assertNotIn("temporary_output", text, path.name)


class ArchiveTests(unittest.TestCase):
    def test_tar_entry_order_ownership_and_modes_are_exact(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stage = root / "stage"
            files = write_stage(stage, "libonnxruntime.so")
            archive = root / "sidecar.tar"
            archive_tool.write_tar(stage, archive, "libonnxruntime.so", 1781827200)
            with tarfile.open(archive, "r:") as bundle:
                members = bundle.getmembers()
            self.assertEqual([member.name for member in members], ["lib", *files])
            self.assertEqual([member.mode for member in members], [0o755, 0o644, 0o644, 0o644, 0o644, 0o755])
            self.assertTrue(members[0].isdir())
            self.assertTrue(all(member.isfile() for member in members[1:]))
            self.assertTrue(all(member.uid == member.gid == 0 for member in members))
            self.assertTrue(all(member.uname == member.gname == "root" for member in members))
            self.assertTrue(all(member.mtime == 1781827200 for member in members))
            extracted = root / "extracted"
            archive_tool.extract_checked("tar.zst", archive, extracted, "libonnxruntime.so")
            for name in files:
                self.assertEqual(
                    extracted.joinpath(*name.split("/")).read_bytes(),
                    stage.joinpath(*name.split("/")).read_bytes(),
                )

    def test_windows_zip_entry_order_modes_and_timestamp_are_exact(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stage = root / "stage"
            files = write_stage(stage, "onnxruntime.dll")
            archive = root / "sidecar.zip"
            archive_tool.write_zip(stage, archive, "onnxruntime.dll", 1781827200)
            with zipfile.ZipFile(archive) as bundle:
                entries = bundle.infolist()
            self.assertEqual([entry.filename for entry in entries], ["lib/", *files])
            self.assertEqual(
                [entry.external_attr >> 16 for entry in entries],
                [0o40755, 0o100644, 0o100644, 0o100644, 0o100644, 0o100755, 0o100644, 0o100755, 0o100755, 0o100755, 0o100755],
            )
            self.assertTrue(all(entry.date_time == (2026, 6, 19, 0, 0, 0) for entry in entries))
            extracted = root / "extracted"
            archive_tool.extract_checked("zip", archive, extracted, "onnxruntime.dll")
            for name in files:
                self.assertEqual(
                    extracted.joinpath(*name.split("/")).read_bytes(),
                    stage.joinpath(*name.split("/")).read_bytes(),
                )

    def test_unexpected_archive_entry_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / "bad.zip"
            with zipfile.ZipFile(archive, "w") as bundle:
                bundle.writestr("unexpected", b"bad")
            with self.assertRaisesRegex(SystemExit, "unexpected sidecar archive entry"):
                archive_tool.extract_checked("zip", archive, root / "out", "onnxruntime.dll")


class StageAndFinalValidationTests(unittest.TestCase):
    def test_official_linux_stage_extracts_only_the_pinned_shape(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            copied_tools = root / "tools"
            shutil.copytree(TOOLS, copied_tools)
            cache = root / "cache"
            destination = root / "stage"
            cache.mkdir()
            upstream = cache / "onnxruntime-linux-x64-1.27.0.tgz"
            expected_root = "onnxruntime-linux-x64-1.27.0"
            files = {
                "lib/libonnxruntime.so.1.27.0": b"runtime",
                "LICENSE": b"license\r\n",
                "ThirdPartyNotices.txt": b"notices\r\n",
                "VERSION_NUMBER": f"{VERSION}\r\n".encode(),
                "GIT_COMMIT_ID": f"{COMMIT}\r\n".encode(),
            }
            with tarfile.open(upstream, "w:gz") as bundle:
                directory = tarfile.TarInfo(expected_root)
                directory.type = tarfile.DIRTYPE
                bundle.addfile(directory)
                for name, content in files.items():
                    info = tarfile.TarInfo(f"{expected_root}/{name}")
                    info.size = len(content)
                    bundle.addfile(info, io.BytesIO(content))
            upstream_sha256 = hashlib.sha256(upstream.read_bytes()).hexdigest()
            source_inputs = copied_tools / "source_inputs.sh"
            source_inputs.write_text(
                source_inputs.read_text().replace(
                    "547e40a48f1fe73e3f812d7c88a948612c23f896b91e4e2ee1e232d7b468246f",
                    upstream_sha256,
                )
            )
            subprocess.run(
                [
                    "bash",
                    str(copied_tools / "stage_official.sh"),
                    "linux-x64",
                    str(destination),
                    str(cache),
                ],
                check=True,
                capture_output=True,
                text=True,
            )
            self.assertEqual((destination / "LICENSE").read_bytes(), b"license\n")
            self.assertEqual(
                (destination / "ThirdPartyNotices.txt").read_bytes(), b"notices\n"
            )
            self.assertEqual(
                (destination / "VERSION_NUMBER").read_bytes(), f"{VERSION}\n".encode()
            )
            self.assertEqual(
                (destination / "GIT_COMMIT_ID").read_bytes(), f"{COMMIT}\n".encode()
            )
            self.assertEqual(
                (destination / "lib" / "libonnxruntime.so").read_bytes(), b"runtime"
            )
            self.assertEqual(
                (destination / "lib" / "libonnxruntime.so").stat().st_mode & 0o777,
                0o755,
            )

    @unittest.skipUnless(shutil.which("zstd"), "zstd is required for tar.zst validation")
    def test_final_archive_validator_runs_as_an_independent_program(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            copied_tools = root / "tools"
            shutil.copytree(TOOLS, copied_tools)
            stage = root / "stage"
            stage.joinpath("lib").mkdir(parents=True)
            documents = {
                "LICENSE": b"fixture license\n",
                "ThirdPartyNotices.txt": b"fixture notices\n",
                "VERSION_NUMBER": f"{VERSION}\n".encode(),
                "GIT_COMMIT_ID": f"{COMMIT}\n".encode(),
            }
            for name, content in documents.items():
                (stage / name).write_bytes(content)
            source_inputs = copied_tools / "source_inputs.sh"
            replace_assignment(
                source_inputs,
                "ONNXRUNTIME_LICENSE_SHA256",
                hashlib.sha256(documents["LICENSE"]).hexdigest(),
            )
            replace_assignment(
                source_inputs,
                "ONNXRUNTIME_NOTICES_SHA256",
                hashlib.sha256(documents["ThirdPartyNotices.txt"]).hexdigest(),
            )
            runtime = bytearray(1_048_576)
            runtime[:6] = b"\x7fELF\x02\x01"
            struct.pack_into("<HH", runtime, 16, 3, 183)
            runtime[128 : 128 + len(VERSION)] = VERSION.encode()
            runtime[160:170] = b"GLIBC_2.17"
            (stage / "lib" / "libonnxruntime.so").write_bytes(runtime)
            archive = root / "ctx-onnxruntime-linux-aarch64.tar.zst"
            subprocess.run(
                [
                    "python3",
                    str(copied_tools / "archive_tool.py"),
                    "create",
                    "--kind",
                    "tar.zst",
                    "--library",
                    "libonnxruntime.so",
                    "--source",
                    str(stage),
                    "--output",
                    str(archive),
                    "--source-date-epoch",
                    "1781827200",
                ],
                check=True,
            )
            result = subprocess.run(
                [
                    "bash",
                    str(copied_tools / "validate_sidecar.sh"),
                    "linux-aarch64",
                    str(archive),
                ],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(
                "native ONNX Runtime load check skipped", result.stdout
            )
            self.assertIn(
                "ONNX Runtime sidecar ok: linux-aarch64 version=1.27.0",
                result.stdout,
            )


class RuntimeValidationTests(unittest.TestCase):
    def validate(self, platform: str, data: bytes, max_glibc: str = "2.39") -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "runtime"
            path.write_bytes(data)
            validate_runtime.validate_binary(
                platform,
                path,
                VERSION,
                max_glibc,
                "ctx-freebsd-source-v1",
                "source-sha",
                "ports-commit",
                "deps-sha",
                "14",
            )

    def elf(self, machine: int, osabi: int = 0) -> bytearray:
        data = bytearray(1024)
        data[:6] = b"\x7fELF\x02\x01"
        data[7] = osabi
        struct.pack_into("<HH", data, 16, 3, machine)
        data[128 : 128 + len(VERSION)] = VERSION.encode()
        return data

    def test_linux_architectures_and_glibc_ceiling(self) -> None:
        x64 = self.elf(62)
        x64[160:170] = b"GLIBC_2.39"
        self.validate("linux-x64", bytes(x64))
        arm64 = self.elf(183)
        arm64[160:170] = b"GLIBC_2.17"
        self.validate("linux-aarch64", bytes(arm64))
        too_new = self.elf(62)
        too_new[160:170] = b"GLIBC_2.40"
        with self.assertRaisesRegex(SystemExit, "newer than allowed"):
            self.validate("linux-x64", bytes(too_new))

    def test_freebsd_provenance_is_required(self) -> None:
        data = self.elf(62, 9)
        markers = validate_runtime.freebsd_provenance(
            "ctx-freebsd-source-v1", "source-sha", "ports-commit", "deps-sha", "14"
        )
        data.extend("|".join(markers).encode())
        self.validate("freebsd-x64", bytes(data))
        with self.assertRaisesRegex(SystemExit, "missing pinned build provenance"):
            self.validate("freebsd-x64", bytes(self.elf(62, 9)))

    def test_macho_architectures_and_pe_dll(self) -> None:
        for platform, cpu in (("macos-arm64", 0x0100000C), ("macos-x64", 0x01000007)):
            data = bytearray(128)
            struct.pack_into("<IIII", data, 0, 0xFEEDFACF, cpu, 0, 6)
            data[64 : 64 + len(VERSION)] = VERSION.encode()
            self.validate(platform, bytes(data))

        data = bytearray(256)
        data[:2] = b"MZ"
        struct.pack_into("<I", data, 0x3C, 0x80)
        data[0x80:0x84] = b"PE\0\0"
        struct.pack_into("<H", data, 0x84, 0x8664)
        struct.pack_into("<H", data, 0x80 + 22, 0x2000)
        struct.pack_into("<H", data, 0x80 + 24, 0x20B)
        data[0xC0 : 0xC0 + len(VERSION)] = VERSION.encode()
        self.validate("windows-x64", bytes(data))


if __name__ == "__main__":
    unittest.main()
