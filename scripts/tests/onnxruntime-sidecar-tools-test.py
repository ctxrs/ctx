#!/usr/bin/env python3

import base64
import hashlib
import importlib.util
import io
import json
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
from argparse import Namespace
from pathlib import Path
from unittest import mock


sys.dont_write_bytecode = True

REPO_ROOT = Path(__file__).resolve().parents[2]
TOOLS = REPO_ROOT / "scripts" / "onnxruntime-sidecar"
VERSION = "1.27.0"
COMMIT = "8f0278c77bf44b0cc83c098c6c722b92a36ac4b5"
CUDA_DEPENDENCY_LIBRARIES = (
    "libcudart.so.12",
    "libcublasLt.so.12",
    "libcublas.so.12",
    "libcurand.so.10",
    "libcufft.so.11",
    "libnvrtc.so.12",
    "libcudnn.so.9",
    "libcudnn_graph.so.9",
    "libcudnn_ops.so.9",
)
WINDOWS_ML_FILES = (
    "LICENSE",
    "ThirdPartyNotices.txt",
    "lib/DirectML.dll",
    "lib/Microsoft.Windows.AI.MachineLearning.dll",
    "lib/onnxruntime.dll",
)
WINDOWS_ML_VERSION = "2.1.74"
WINDOWS_ML_ORT_VERSION = "1.24.6"


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


archive_tool = load_module("archive_tool", TOOLS / "archive_tool.py")
validate_runtime = load_module("validate_runtime", TOOLS / "validate_runtime.py")
semantic_release_assets = load_module(
    "semantic_release_assets", REPO_ROOT / "scripts" / "semantic-release-assets.py"
)


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


def semantic_catalog_fixture(asset_id: str) -> dict[str, object]:
    expected = semantic_release_assets.EXPECTED_ASSETS[asset_id]
    if expected["files"] is None:
        paths = sorted(
            (
                "LICENSES/MODEL_LICENSE.txt",
                "PROVENANCE.json",
                "THIRD_PARTY_NOTICES.md",
                "document.mlpackage/Data/model.bin",
                "manifest.json",
                "query.mlpackage/Data/model.bin",
                "tokenizer.json",
            )
        )
    else:
        paths = sorted(expected["files"])
    records = {
        path: {"path": path, "sha256": "1" * 64, "size": 1} for path in paths
    }
    if asset_id in ("onnx_model", "onnx_model_o4_fp16"):
        variant = (
            "cpu-fp32" if asset_id == "onnx_model" else "accelerator-o4-fp16"
        )
        for path, (size, digest) in semantic_release_assets.model_required_files(
            variant
        ).items():
            records[path] = {"path": path, "sha256": digest, "size": size}
        manifest = semantic_release_assets.model_manifest(variant)
        records["manifest.json"] = {
            "path": "manifest.json",
            "sha256": hashlib.sha256(manifest).hexdigest(),
            "size": len(manifest),
        }
    if asset_id == "apple_coreml":
        records["manifest.json"]["sha256"] = (
            semantic_release_assets.COREML_MANIFEST_SHA256
        )
    asset = {
        key: expected[key]
        for key in (
            "archive_format",
            "archive_path_prefix",
            "artifact",
            "backend",
            "max_expanded_bytes",
            "max_files",
            "platform",
            "role",
            "version",
        )
    }
    asset["archive_sha256"] = (
        semantic_release_assets.COREML_ARCHIVE_SHA256
        if asset_id == "apple_coreml"
        else "a" * 64
    )
    asset["files"] = [records[path] for path in sorted(records)]
    return {"asset": asset, "id": asset_id}


class ManifestTests(unittest.TestCase):
    def test_all_platform_release_shapes_are_exact(self) -> None:
        expected = {
            "linux-x64": ("ctx-onnxruntime-linux-x64.tar.zst", "tar.zst", "libonnxruntime.so", "official"),
            "linux-x64-cuda12": ("ctx-onnxruntime-linux-x64-cuda12.tar.zst", "tar.zst", "libonnxruntime.so", "official"),
            "linux-aarch64": ("ctx-onnxruntime-linux-aarch64.tar.zst", "tar.zst", "libonnxruntime.so", "official"),
            "macos-arm64": ("ctx-onnxruntime-macos-arm64.tar.zst", "tar.zst", "libonnxruntime.dylib", "official"),
            "macos-x64": ("ctx-onnxruntime-macos-x64.tar.zst", "tar.zst", "libonnxruntime.dylib", "macos-x64-source"),
            "windows-x64": ("ctx-onnxruntime-windows-x64.zip", "zip", "onnxruntime.dll", "official"),
            "windows-x64-windowsml": ("ctx-windowsml-windows-x64.zip", "zip", "Microsoft.Windows.AI.MachineLearning.dll", "official"),
            "freebsd-x64": ("ctx-onnxruntime-freebsd-x64.tar.zst", "tar.zst", "libonnxruntime.so", "freebsd-x64-source"),
        }
        for platform, shape in expected.items():
            with self.subTest(platform=platform):
                manifest = shell_manifest("release_manifest.sh", platform)
                self.assertEqual(
                    manifest["version"],
                    WINDOWS_ML_VERSION
                    if platform == "windows-x64-windowsml"
                    else VERSION,
                )
                self.assertEqual(manifest["api_version"], "24")
                self.assertEqual(
                    manifest["commit"],
                    "" if platform == "windows-x64-windowsml" else COMMIT,
                )
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
                self.assertEqual(
                    manifest["semantic_catalog_asset"],
                    "0" if platform == "windows-x64" else "1",
                )
        cuda = shell_manifest("release_manifest.sh", "linux-x64-cuda12")
        self.assertEqual(cuda["runtime_backend"], "cuda")
        self.assertEqual(cuda["catalog_role"], "accelerator")
        self.assertEqual(cuda["catalog_backend"], "ort-cuda")
        self.assertEqual(
            cuda["provider_libraries"],
            " ".join(
                (
                    "libonnxruntime_providers_shared.so",
                    "libonnxruntime_providers_cuda.so",
                    *CUDA_DEPENDENCY_LIBRARIES,
                )
            ),
        )
        windows_ml = shell_manifest("release_manifest.sh", "windows-x64-windowsml")
        self.assertEqual(windows_ml["version"], WINDOWS_ML_VERSION)
        self.assertEqual(windows_ml["catalog_backend"], "windows-ml")
        self.assertEqual(windows_ml["archive_exact_files"], " ".join((
            "LICENSE",
            "ThirdPartyNotices.txt",
            "lib/Microsoft.Windows.AI.MachineLearning.dll",
            "lib/onnxruntime.dll",
            "lib/DirectML.dll",
        )))

    def test_source_input_manifest_preserves_every_pin(self) -> None:
        manifest = shell_manifest("source_inputs.sh", "linux-x64")
        expected = {
            "source_sha256": "b41d09905a3c2f3a25709d1dcce8ef3942a4c2799d1046f74be7b6bbebc45e6a",
            "license_sha256": "2f07c72751aed99790b8a4869cf2311df85a860b22ded05fa22803587a48922c",
            "notices_sha256": "0e07b95f3a8d6230037707c5c4a2b554d12c4cb67369669ac255635528ffcee2",
            "deps_sha256": "e411468ead299e3386b2e5e9d773e50e1939b5fc0baca599666ca5757eeb3f71",
            "nvidia_cublas_sha256": "e4f53a8ca8c5d6e8c492d0d0a3d565ecb59a751b19cfdaa4f6da0ab2104c1702",
            "nvidia_cuda_runtime_sha256": "25bba2dfb01d48a9b59ca474a1ac43c6ebf7011f1b0b8cc44f54eb6ac48a96c3",
            "nvidia_cuda_nvrtc_sha256": "210cf05005a447e29214e9ce50851e83fc5f4358df8b453155d5e1918094dcb4",
            "nvidia_curand_sha256": "49b274db4780d421bd2ccd362e1415c13887c53c214f0d4b761752b8f9f6aa1e",
            "nvidia_cufft_sha256": "c67884f2a7d276b4b80eb56a79322a95df592ae5e765cf1243693365ccab4e28",
            "nvidia_cudnn_sha256": "4ea1ba443fa28ac6cf04b7a44a107dfd54cf355c2324938102ddb21778ab10ce",
            "nvidia_cuda_license_sha256": "ad6f5853fba0ca0d159d0f58d49ae49830c2f8c93f7a92648b9ce90adb4c6ccd",
            "nvidia_cudnn_license_sha256": "49cf79bdb35734b52fe6203013b3bd759f81e998cd32aa2c65c51db9a88c61d2",
            "windows_vc_runtime_version": "14.44.35211.0",
            "windows_vc_redist_url": "https://download.visualstudio.microsoft.com/download/pr/7ebf5fdb-36dc-4145-b0a0-90d3d5990a61/CC0FF0EB1DC3F5188AE6300FAEF32BF5BEEBA4BDD6E8E445A9184072096B713B/VC_redist.x64.exe",
            "windows_vc_redist_sha256": "cc0ff0eb1dc3f5188ae6300faef32bf5beeba4bdd6e8e445a9184072096b713b",
            "windows_vc_minimum_cab_sha256": "640aa6c516c72444523b8fbe034db46ff4e118ed02705340e3ccb62d426ff040",
            "windows_vc_license_sha256": "8099dc3cf9502c335da829e5c755948a12e3e6de490eb492a99deb673d883d8b",
            "windows_msvcp140_sha256": "0f885b509a685d2bbfa652fed26b5fb31d88fbdab0a978c641d1c7b8aa460aa9",
            "windows_msvcp140_1_sha256": "bfad5aef4c63a669e3c140655cdfdf395b6c979b400a447bd5dcb65ed8826c3d",
            "windows_vcruntime140_sha256": "d5e4d9a3e835fa679450145d6a7d94e36573a509317111904d9b3712c30d9066",
            "windows_vcruntime140_1_sha256": "1f2d41c4aa5db0bc33ebf7b66d72943a817d7ce6cbe880502a9403823633093f",
            "windows_ml_version": WINDOWS_ML_VERSION,
            "windows_ml_onnxruntime_version": WINDOWS_ML_ORT_VERSION,
            "windows_ml_nuget_sha256": "691165fa3c07a04b752cbf4a07e93ed13a418e9dea1ee89eb163d2225e2ba3af",
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
            "linux-x64-cuda12": (
                "onnxruntime-linux-x64-gpu_cuda12-1.27.0.tgz",
                "3fed2d2f45f01f8bc1c1597a31afe29efd692c7ea4648d58e1844a8a0d0a48cb",
                "onnxruntime-linux-x64-gpu_cuda12-1.27.0",
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
    def test_cuda_archive_shape_is_exactly_eighteen_files(self) -> None:
        libraries = (
            "libonnxruntime_providers_shared.so",
            "libonnxruntime_providers_cuda.so",
            *CUDA_DEPENDENCY_LIBRARIES,
        )
        files = archive_tool.archive_files(
            "libonnxruntime.so",
            libraries,
            ("NVIDIA-CUDA-LICENSE.txt", "NVIDIA-CUDNN-LICENSE.txt"),
        )
        self.assertEqual(len(files), 18)
        self.assertEqual(len(set(files)), 18)

    def test_windows_ml_zip_shape_is_exactly_five_files(self) -> None:
        self.assertEqual(
            archive_tool.archive_files(
                "Microsoft.Windows.AI.MachineLearning.dll",
                ("onnxruntime.dll", "DirectML.dll"),
                exact_files=WINDOWS_ML_FILES,
            ),
            WINDOWS_ML_FILES,
        )

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

    def test_zip_entry_order_modes_and_timestamp_are_exact(self) -> None:
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
                [
                    0o40755,
                    0o100644,
                    0o100644,
                    0o100644,
                    0o100644,
                    0o100755,
                    0o100644,
                    0o100755,
                    0o100755,
                    0o100755,
                    0o100755,
                ],
            )
            self.assertTrue(all(entry.date_time == (2026, 6, 19, 0, 0, 0) for entry in entries))
            extracted = root / "extracted"
            archive_tool.extract_checked("zip", archive, extracted, "onnxruntime.dll")
            for name in files:
                self.assertEqual(
                    extracted.joinpath(*name.split("/")).read_bytes(),
                    stage.joinpath(*name.split("/")).read_bytes(),
                )

    def test_raw_noncanonical_archive_components_are_rejected(self) -> None:
        for raw in (
            "./lib/onnxruntime.dll",
            "lib//onnxruntime.dll",
            "lib/../onnxruntime.dll",
            "lib/./onnxruntime.dll",
        ):
            with self.subTest(raw=raw):
                with self.assertRaisesRegex(
                    SystemExit, "unsafe sidecar archive path"
                ):
                    archive_tool.canonical_name(raw)
                with self.assertRaisesRegex(
                    semantic_release_assets.AssetError,
                    "unsafe model archive path|unsafe asset path",
                ):
                    semantic_release_assets.canonical_tar_name(raw)

    def test_unexpected_archive_entry_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / "bad.zip"
            with zipfile.ZipFile(archive, "w") as bundle:
                bundle.writestr("unexpected", b"bad")
            with self.assertRaisesRegex(SystemExit, "unexpected sidecar archive entry"):
                archive_tool.extract_checked("zip", archive, root / "out", "onnxruntime.dll")


class SemanticReleaseAssetTests(unittest.TestCase):
    def test_nested_asset_records_compare_as_one_sorted_inventory(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            payloads = {
                "LICENSES/MODEL_LICENSE.txt": b"license\n",
                "PROVENANCE.json": b"{}\n",
                "document.mlpackage/Data/model.bin": b"model",
            }
            for relative, body in payloads.items():
                path = root.joinpath(*relative.split("/"))
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(body)
            paths = sorted(payloads)
            records = semantic_release_assets.collect_records(root, paths)
            self.assertEqual(
                [record["path"] for record in records],
                paths,
            )

    def test_model_constructor_emits_both_exact_archive_contracts(self) -> None:
        for variant, artifact, asset_id in (
            (
                "cpu-fp32",
                "ctx-multilingual-e5-small-onnx-fp32-1.0.0.tar.xz",
                "onnx_model",
            ),
            (
                "accelerator-o4-fp16",
                "ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0.tar.xz",
                "onnx_model_o4_fp16",
            ),
        ):
            with self.subTest(variant=variant), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                source = root / "source"
                output = root / "output"
                payloads = {
                    "LICENSE": b"fixture model license\n",
                    "config.json": b"config",
                    "onnx/model.onnx": f"onnx:{variant}".encode(),
                    "special_tokens_map.json": b"special",
                    "tokenizer.json": b"tokenizer",
                    "tokenizer_config.json": b"tokenizer config",
                }
                for relative, body in payloads.items():
                    path = source.joinpath(*relative.split("/"))
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(body)
                common = {
                    relative: (len(payloads[relative]), hashlib.sha256(payloads[relative]).hexdigest())
                    for relative in (
                        "config.json",
                        "special_tokens_map.json",
                        "tokenizer.json",
                        "tokenizer_config.json",
                    )
                }
                selected = {
                    "asset_id": asset_id,
                    "artifact": artifact,
                    "onnx_size": len(payloads["onnx/model.onnx"]),
                    "onnx_sha256": hashlib.sha256(
                        payloads["onnx/model.onnx"]
                    ).hexdigest(),
                }
                with mock.patch.dict(
                    semantic_release_assets.COMMON_MODEL_FILES, common, clear=True
                ), mock.patch.dict(
                    semantic_release_assets.MODEL_VARIANTS,
                    {variant: selected},
                    clear=True,
                ):
                    semantic_release_assets.build_model(
                        Namespace(variant=variant, source=source, output_dir=output)
                    )
                    records = semantic_release_assets.validate_model_archive(
                        output / artifact, variant
                    )
                self.assertEqual(
                    [record["path"] for record in records],
                    list(semantic_release_assets.MODEL_PATHS),
                )
                metadata = json.loads((output / f"{artifact}.asset.json").read_bytes())
                self.assertEqual(metadata["id"], asset_id)
                self.assertEqual(metadata["asset"]["artifact"], artifact)
                self.assertEqual(metadata["asset"]["archive_format"], "tar.xz")
                self.assertEqual(metadata["asset"]["archive_path_prefix"], artifact[:-7])

    def test_catalog_uses_only_canonical_padded_base64_semantic_fields(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            records = []
            for asset_id in sorted(semantic_release_assets.EXPECTED_ASSET_IDS):
                path = root / f"{asset_id}.json"
                path.write_bytes(
                    semantic_release_assets.canonical_json(
                        semantic_catalog_fixture(asset_id)
                    )
                    + b"\n"
                )
                records.append(path)
            output = root / "semantic.env"
            semantic_release_assets.build_catalog(
                Namespace(asset_record=records, output=output)
            )
            fields = dict(
                line.split("=", 1) for line in output.read_text().splitlines()
            )
            self.assertEqual(
                set(fields),
                {
                    "CTX_RELEASE_SEMANTIC_SCHEMA_VERSION",
                    "CTX_RELEASE_SEMANTIC_ASSETS",
                    "CTX_RELEASE_SEMANTIC_AUTHORITY_apple_silicon_coreml",
                    "CTX_RELEASE_SEMANTIC_AUTHORITY_windows_windows_ml",
                    "CTX_RELEASE_SEMANTIC_AUTHORITY_linux_nvidia_ort_cuda",
                    "CTX_RELEASE_SEMANTIC_AUTHORITY_universal_ort_cpu",
                },
            )
            self.assertEqual(fields["CTX_RELEASE_SEMANTIC_SCHEMA_VERSION"], "1")
            for name, encoded in fields.items():
                if name == "CTX_RELEASE_SEMANTIC_SCHEMA_VERSION":
                    continue
                decoded = base64.b64decode(encoded, validate=True)
                self.assertEqual(
                    base64.b64encode(decoded).decode("ascii"),
                    encoded,
                )
                self.assertEqual(
                    semantic_release_assets.canonical_json(json.loads(decoded)),
                    decoded,
                )
            authorities = {
                name: json.loads(base64.b64decode(value, validate=True))
                for name, value in fields.items()
                if "_AUTHORITY_" in name
            }
            assets = json.loads(
                base64.b64decode(
                    fields["CTX_RELEASE_SEMANTIC_ASSETS"], validate=True
                )
            )["assets"]
            self.assertEqual(len(assets), 10)
            self.assertEqual(
                authorities[
                    "CTX_RELEASE_SEMANTIC_AUTHORITY_apple_silicon_coreml"
                ]["asset_ids"],
                ["onnx_model", "macos_arm64_cpu", "apple_coreml"],
            )
            self.assertEqual(
                authorities[
                    "CTX_RELEASE_SEMANTIC_AUTHORITY_windows_windows_ml"
                ]["asset_ids"],
                ["onnx_model_o4_fp16", "windows_ml"],
            )
            self.assertEqual(
                authorities[
                    "CTX_RELEASE_SEMANTIC_AUTHORITY_linux_nvidia_ort_cuda"
                ]["asset_ids"],
                ["onnx_model_o4_fp16", "linux_cuda12"],
            )
            self.assertEqual(
                authorities[
                    "CTX_RELEASE_SEMANTIC_AUTHORITY_universal_ort_cpu"
                ]["asset_ids"],
                [
                    "onnx_model",
                    "linux_x64_cpu",
                    "linux_aarch64_cpu",
                    "macos_arm64_cpu",
                    "macos_x64_cpu",
                    "windows_ml",
                    "freebsd_x64_cpu",
                ],
            )

    def test_catalog_rejects_incomplete_client_asset_schema(self) -> None:
        record = semantic_catalog_fixture("linux_x64_cpu")
        del record["asset"]["max_files"]
        with self.assertRaisesRegex(
            semantic_release_assets.AssetError, "invalid Semantic asset fields"
        ):
            semantic_release_assets.validate_asset_record(
                record["id"], record["asset"]
            )

    def test_catalog_rejects_noncanonical_client_paths(self) -> None:
        for path in ("./LICENSE", "lib//runtime", "lib/../runtime", "NUL.txt"):
            record = semantic_catalog_fixture("linux_x64_cpu")
            record["asset"]["files"][0]["path"] = path
            with self.subTest(path=path), self.assertRaises(
                semantic_release_assets.AssetError
            ):
                semantic_release_assets.validate_asset_record(
                    record["id"], record["asset"]
                )


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

    def test_final_archive_validator_runs_as_an_independent_program(self) -> None:
        self.assertIsNotNone(
            shutil.which("zstd"),
            "zstd is required; the release archive test must fail rather than skip",
        )
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

    def test_cuda_static_dependency_closure_rejects_missing_user_space_library(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in ("libprovider.so", "libcublas.so.12"):
                (root / name).write_bytes(b"ELF fixture")
            complete = mock.Mock(
                stdout=(
                    " 0x1 (NEEDED) Shared library: [libcublas.so.12]\n"
                    " 0x1 (NEEDED) Shared library: [libc.so.6]\n"
                )
            )
            leaf = mock.Mock(
                stdout=" 0x1 (NEEDED) Shared library: [libc.so.6]\n"
            )
            with mock.patch.object(
                validate_runtime.subprocess,
                "run",
                side_effect=(complete, leaf),
            ):
                validate_runtime.validate_elf_dependency_closure(
                    root, ["libprovider.so", "libcublas.so.12"]
                )
            missing = mock.Mock(
                stdout=" 0x1 (NEEDED) Shared library: [libcudnn_adv.so.9]\n"
            )
            with mock.patch.object(
                validate_runtime.subprocess, "run", return_value=missing
            ), self.assertRaisesRegex(SystemExit, "unresolved ELF dependencies"):
                validate_runtime.validate_elf_dependency_closure(
                    root, ["libprovider.so"]
                )

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
