#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile


SCRIPT = Path(__file__).resolve().parents[1] / "release-sbom.py"
COMMIT = "0123456789abcdef0123456789abcdef01234567"
SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
TANTIVY_FEATURES = (
    "columnar-zstd-compression",
    "fs4",
    "lz4-compression",
    "lz4_flex",
    "memmap2",
    "mmap",
    "tempfile",
    "zstd",
    "zstd-compression",
)
WORKSPACE_PACKAGES = (
    ("ctx", "crates/ctx-cli"),
    ("ctx-history-core", "crates/ctx-history-core"),
    ("ctx-history-index", "crates/ctx-history-index"),
)
EXTERNAL_PACKAGES = (
    ("fs4", "0.1.0"),
    ("lz4_flex", "0.11.0"),
    ("memmap2", "0.9.0"),
    ("tantivy", "0.26.1"),
    ("tempfile", "3.0.0"),
    ("zstd", "0.13.0"),
)
LEGACY_RELEASE_ASSETS = (
    "ctx-linux-x64",
    "ctx-linux-x64.cdx.json",
    "ctx-linux-x64.third-party-notices.txt",
    "ctx-linux-aarch64",
    "ctx-linux-aarch64.cdx.json",
    "ctx-linux-aarch64.third-party-notices.txt",
    "ctx-macos-arm64",
    "ctx-macos-arm64.cdx.json",
    "ctx-macos-arm64.third-party-notices.txt",
    "ctx-macos-x64",
    "ctx-macos-x64.cdx.json",
    "ctx-macos-x64.third-party-notices.txt",
    "ctx-windows-x64.exe",
    "ctx-windows-x64.exe.cdx.json",
    "ctx-windows-x64.exe.third-party-notices.txt",
    "ctx-freebsd-x64",
    "ctx-freebsd-x64.cdx.json",
    "ctx-freebsd-x64.third-party-notices.txt",
    "ctx-onnxruntime-linux-x64.tar.gz",
    "ctx-onnxruntime-linux-aarch64.tar.gz",
    "ctx-onnxruntime-macos-arm64.tar.gz",
    "ctx-onnxruntime-macos-x64.tar.gz",
    "ctx-onnxruntime-windows-x64.zip",
    "ctx-onnxruntime-freebsd-x64.tar.gz",
)
WINDOWS_RUNTIME_FILES = (
    "LICENSE",
    "ThirdPartyNotices.txt",
    "VERSION_NUMBER",
    "GIT_COMMIT_ID",
    "MICROSOFT_VC_RUNTIME_LICENSE.rtf",
    "lib/onnxruntime.dll",
    "lib/msvcp140.dll",
    "lib/msvcp140_1.dll",
    "lib/vcruntime140.dll",
    "lib/vcruntime140_1.dll",
)
RELEASE_AUTHORITY_CANDIDATES = (
    "ctx.candidate.json",
    "ctx-linux-aarch64.candidate.json",
    "ctx-macos-arm64.candidate.json",
    "ctx-macos-x64.candidate.json",
    "ctx.exe.candidate.json",
    "ctx-freebsd-x64.candidate.json",
)


class ReleaseSbomTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.runfiles = self.root / "runfiles"
        self.main_runfiles = self.runfiles / "_main"
        self.main_runfiles.mkdir(parents=True)
        self.target_id = "linux-x64"
        self.platform = "linux-x64"

        self.artifact = self.root / "ctx"
        self.artifact.write_bytes(b"exact release artifact\n")
        self.cargo_lock = self.root / "Cargo.lock"
        self.cargo_lock.write_text(self.lock_text(), encoding="utf-8")
        self.module_file = self.root / "MODULE.bazel"
        self.module_file.write_text('module(name = "ctx")\n', encoding="utf-8")
        self.module_lock = self.root / "MODULE.bazel.lock"
        self.module_lock.write_text('{"lockFileVersion":21}\n', encoding="utf-8")
        self.candidate_schema = self.root / "candidate.schema.json"
        self.candidate_schema.write_text('{"type":"object"}\n', encoding="utf-8")
        self.target_matrix = self.root / "release-targets-v1.json"
        self.target_matrix.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "targets": [
                        {
                            "id": "linux-x64",
                            "public_rust_target": "x86_64-unknown-linux-gnu",
                            "public_construction_authority": "bazel-release-route-v1",
                            "public_construction_label": "//:ctx_release_linux_x64",
                        }
                    ],
                },
                sort_keys=True,
                separators=(",", ":"),
            )
            + "\n",
            encoding="utf-8",
        )

        self.workspace_manifest = self.main_runfiles / "Cargo.toml"
        self.workspace_manifest.write_text(
            """\
[workspace]
members = [
  "crates/ctx-cli",
  "crates/ctx-history-core",
  "crates/ctx-history-index",
]

[workspace.package]
version = "0.26.0"
license = "MIT"
repository = "https://github.com/ctxrs/ctx"

[workspace.dependencies]
tantivy = { version = "0.26.1", default-features = false, features = ["mmap", "lz4-compression", "zstd-compression", "columnar-zstd-compression"] }
""",
            encoding="utf-8",
        )
        (self.main_runfiles / "LICENSE").write_text(
            "Synthetic workspace MIT license.\n", encoding="utf-8"
        )
        for name, directory in WORKSPACE_PACKAGES:
            manifest = self.main_runfiles / directory / "Cargo.toml"
            manifest.parent.mkdir(parents=True)
            dependencies = (
                "\n[dependencies]\ntantivy.workspace = true\n"
                if name == "ctx-history-index"
                else ""
            )
            manifest.write_text(
                f"""\
[package]
name = "{name}"
version.workspace = true
license.workspace = true
repository.workspace = true
{dependencies}""",
                encoding="utf-8",
            )
        self.index_manifest = (
            self.main_runfiles / "crates/ctx-history-index/Cargo.toml"
        )

        for name, version in EXTERNAL_PACKAGES:
            repository = f"rules_rust~~crate~crates__{name}-{version}"
            manifest = self.runfiles / repository / "Cargo.toml"
            manifest.parent.mkdir(parents=True)
            manifest.write_text(
                f"""\
[package]
name = "{name}"
version = "{version}"
license = "MIT OR Apache-2.0"
repository = "https://example.invalid/{name}"
""",
                encoding="utf-8",
            )
            (manifest.parent / "LICENSE").write_text(
                f"Synthetic license text for {name}.\n", encoding="utf-8"
            )

        inventory_labels = [
            "@@//crates/ctx-cli:ctx",
            "@@//crates/ctx-history-core:ctx_history_core",
            "@@//crates/ctx-history-index:ctx_history_index",
        ]
        inventory_labels.extend(
            f"@@rules_rust~~crate~crates__{name}-{version}//:{name}"
            for name, version in EXTERNAL_PACKAGES
        )
        self.target_inventory = self.root / "target-dependency-inventory.txt"
        self.target_inventory.write_text(
            "\n".join(sorted(inventory_labels)) + "\n", encoding="utf-8"
        )

        material_lines = [
            "main\tCargo.toml",
            "main\tLICENSE",
        ]
        material_lines.extend(
            f"main\t{directory}/Cargo.toml" for _, directory in WORKSPACE_PACKAGES
        )
        for name, version in EXTERNAL_PACKAGES:
            repository = f"rules_rust~~crate~crates__{name}-{version}"
            material_lines.extend(
                (
                    f"external\t{repository}/Cargo.toml",
                    f"external\t{repository}/LICENSE",
                )
            )
        tantivy_label = (
            "@@rules_rust~~crate~crates__tantivy-0.26.1//:tantivy"
        )
        material_lines.extend(
            f"feature\t{tantivy_label}\t{feature}"
            for feature in TANTIVY_FEATURES
        )
        self.license_materials = self.root / "license-materials.txt"
        self.license_materials.write_text(
            "\n".join(sorted(material_lines)) + "\n", encoding="utf-8"
        )

        self.build_info = self.root / "ctx.build-info.json"
        self.write_build_info()
        self.sbom = self.root / "ctx.cdx.json"
        self.notices = self.root / "ctx.third-party-notices.txt"
        self.size_report = self.root / "ctx.size.json"
        self.candidate = self.root / "ctx.candidate.json"
        self.release_sums = self.root / "SHA256SUMS"
        self.runtime = self.root / "ctx-onnxruntime-windows-x64.zip"
        self.bound_candidate = self.root / "ctx.release-candidate.json"
        self.bound_digest = self.root / "ctx.release-candidate.json.sha256"
        self.handoff = self.root / "release-authority-handoff"
        self.expected_digest: str | None = None

    def tearDown(self) -> None:
        self.temporary.cleanup()

    @staticmethod
    def package(
        name: str,
        version: str,
        dependencies: tuple[str, ...] = (),
        external: bool = False,
    ) -> str:
        lines = ["[[package]]", f'name = "{name}"', f'version = "{version}"']
        if external:
            lines.extend(
                (
                    f'source = "{SOURCE}"',
                    f'checksum = "{hashlib.sha256(f"{name}-{version}".encode()).hexdigest()}"',
                )
            )
        if dependencies:
            lines.append("dependencies = [")
            lines.extend(f' "{dependency}",' for dependency in dependencies)
            lines.append("]")
        return "\n".join(lines)

    def lock_text(self) -> str:
        packages = [
            self.package(
                "ctx",
                "0.26.0",
                (
                    "ctx-history-core",
                    "ctx-history-index",
                ),
            ),
            self.package("ctx-history-core", "0.26.0"),
            self.package(
                "ctx-history-index",
                "0.26.0",
                ("tantivy 0.26.1",),
            ),
            self.package("fs4", "0.1.0", external=True),
            self.package("lz4_flex", "0.11.0", external=True),
            self.package("memmap2", "0.9.0", external=True),
            self.package(
                "tantivy",
                "0.26.1",
                (
                    "fs4 0.1.0",
                    "lz4_flex 0.11.0",
                    "memmap2 0.9.0",
                    "tempfile 3.0.0",
                    "zstd 0.13.0",
                ),
                external=True,
            ),
            self.package("tempfile", "3.0.0", external=True),
            self.package("zstd", "0.13.0", external=True),
        ]
        return "version = 4\n\n" + "\n\n".join(packages) + "\n"

    def write_build_info(
        self,
        platform: str = "linux-x64",
        target: str = "x86_64-unknown-linux-gnu",
    ) -> None:
        self.build_info.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "artifact_sha256": hashlib.sha256(
                        self.artifact.read_bytes()
                    ).hexdigest(),
                    "cargo_lock_sha256": hashlib.sha256(
                        self.cargo_lock.read_bytes()
                    ).hexdigest(),
                    "platform": platform,
                    "target": target,
                    "source": {"commit": COMMIT, "clean": True},
                    "rust_version": "rustc 1.97.1 (test 2026-07-14)",
                    "builder": {
                        "base_image": {"actual": "sha256:" + "b" * 64},
                        "image_id": "sha256:" + "c" * 64,
                        "recipe_sha256": "d" * 64,
                    },
                },
                sort_keys=True,
                separators=(",", ":"),
            )
            + "\n",
            encoding="utf-8",
        )

    def configure_windows_release(self) -> None:
        self.target_id = "windows-x64"
        self.platform = "windows-x64"
        windows_artifact = self.root / "ctx.exe"
        self.artifact.rename(windows_artifact)
        self.artifact = windows_artifact
        self.build_info = self.root / "ctx.exe.build-info.json"
        self.sbom = self.root / "ctx.exe.cdx.json"
        self.notices = self.root / "ctx.exe.third-party-notices.txt"
        self.size_report = self.root / "ctx.exe.size.json"
        self.candidate = self.root / "ctx.exe.candidate.json"
        self.target_matrix.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "targets": [
                        {
                            "id": "windows-x64",
                            "public_rust_target": "x86_64-pc-windows-gnu",
                            "public_construction_authority": "bazel-release-route-v1",
                            "public_construction_label": "//:ctx_release_windows_x64",
                        }
                    ],
                },
                sort_keys=True,
                separators=(",", ":"),
            )
            + "\n",
            encoding="utf-8",
        )
        self.write_build_info("windows-x64", "x86_64-pc-windows-gnu")
        self.write_runtime()
        self.write_release_sums()

    def write_release_handoff(self) -> None:
        self.handoff.mkdir()
        copies = {
            "ctx.exe": self.artifact,
            "ctx.exe.build-info.json": self.build_info,
            "ctx.exe.cdx.json": self.sbom,
            "ctx.exe.size.json": self.size_report,
            "ctx.exe.third-party-notices.txt": self.notices,
            "SHA256SUMS": self.release_sums,
            "ctx-onnxruntime-windows-x64.zip": self.runtime,
            "ctx.exe.candidate.json": self.bound_candidate,
            "ctx.exe.candidate.json.sha256": self.bound_digest,
        }
        for name, source in copies.items():
            shutil.copyfile(source, self.handoff / name)
        for name in RELEASE_AUTHORITY_CANDIDATES:
            if name == "ctx.exe.candidate.json":
                continue
            payload = b"{}\n"
            (self.handoff / name).write_bytes(payload)
            (self.handoff / f"{name}.sha256").write_text(
                hashlib.sha256(payload).hexdigest() + "\n", encoding="ascii"
            )

    def write_runtime(
        self,
        dll: bytes = b"exact Windows runtime DLL\n",
        dll_name: str = "lib/onnxruntime.dll",
        omit: str | None = None,
        extra: str | None = None,
    ) -> None:
        with zipfile.ZipFile(
            self.runtime, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9
        ) as archive:
            directory = zipfile.ZipInfo(
                "lib/", date_time=(1980, 1, 1, 0, 0, 0)
            )
            directory.compress_type = zipfile.ZIP_DEFLATED
            directory.external_attr = 0o40755 << 16
            archive.writestr(directory, b"")
            for name in WINDOWS_RUNTIME_FILES:
                emitted_name = dll_name if name == "lib/onnxruntime.dll" else name
                if emitted_name == omit:
                    continue
                record = zipfile.ZipInfo(
                    emitted_name, date_time=(1980, 1, 1, 0, 0, 0)
                )
                record.compress_type = zipfile.ZIP_DEFLATED
                record.external_attr = 0o100644 << 16
                payload = dll if name == "lib/onnxruntime.dll" else f"{name}\n".encode()
                archive.writestr(record, payload)
            if extra is not None:
                record = zipfile.ZipInfo(
                    extra, date_time=(1980, 1, 1, 0, 0, 0)
                )
                record.compress_type = zipfile.ZIP_DEFLATED
                record.external_attr = 0o100644 << 16
                archive.writestr(record, b"unexpected\n")

    def write_release_sums(self) -> None:
        values = {
            name: hashlib.sha256(f"synthetic {name}\n".encode()).hexdigest()
            for name in LEGACY_RELEASE_ASSETS
        }
        values["ctx-windows-x64.exe"] = hashlib.sha256(
            self.artifact.read_bytes()
        ).hexdigest()
        values["ctx-onnxruntime-windows-x64.zip"] = hashlib.sha256(
            self.runtime.read_bytes()
        ).hexdigest()
        self.release_sums.write_text(
            "".join(f"{values[name]}  {name}\n" for name in LEGACY_RELEASE_ASSETS),
            encoding="ascii",
        )

    def command(self, mode: str) -> list[str]:
        if mode == "verify-release":
            expected = self.expected_digest or self.bound_digest.read_text(
                encoding="ascii"
            ).strip()
            return [
                sys.executable,
                "-I",
                str(SCRIPT),
                mode,
                "--handoff-dir",
                str(self.handoff),
                "--expected-manifest-sha256",
                expected,
            ]
        command = [
            sys.executable,
            "-I",
            str(SCRIPT),
            mode,
            "--artifact",
            str(self.artifact),
            "--build-info",
            str(self.build_info),
        ]
        if mode in ("verify-bundle", "bind-release"):
            candidate = self.candidate
            command.extend(
                [
                    "--sbom",
                    str(self.sbom),
                    "--notices",
                    str(self.notices),
                    "--size-report",
                    str(self.size_report),
                    "--candidate-manifest",
                    str(candidate),
                ]
            )
            if mode == "bind-release":
                command.extend(
                    [
                        "--release-sums",
                        str(self.release_sums),
                        "--runtime-archive",
                        str(self.runtime),
                    ]
                )
            if mode == "bind-release":
                command.extend(
                    [
                        "--output-manifest",
                        str(self.bound_candidate),
                        "--manifest-sha256-output",
                        str(self.bound_digest),
                    ]
                )
            return command
        command.extend(
            [
                "--product",
                "core",
                "--version",
                "0.26.0",
                "--target-id",
                self.target_id,
                "--platform",
                self.platform,
                "--cargo-lock",
                str(self.cargo_lock),
                "--module-lock",
                str(self.module_lock),
                "--module-file",
                str(self.module_file),
                "--target-inventory",
                str(self.target_inventory),
                "--license-materials",
                str(self.license_materials),
                "--target-matrix",
                str(self.target_matrix),
                "--candidate-schema",
                str(self.candidate_schema),
                "--workspace-manifest",
                str(self.workspace_manifest),
                "--index-manifest",
                str(self.index_manifest),
                "--runfiles-root",
                str(self.runfiles),
                "--candidate-manifest",
                str(self.candidate),
            ]
        )
        if mode == "generate":
            command.extend(
                (
                    "--output",
                    str(self.sbom),
                    "--notices-output",
                    str(self.notices),
                    "--size-report-output",
                    str(self.size_report),
                )
            )
        else:
            command.extend(
                (
                    "--sbom",
                    str(self.sbom),
                    "--notices",
                    str(self.notices),
                    "--size-report",
                    str(self.size_report),
                )
            )
        return command

    def run_command(
        self, mode: str, check: bool = True
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            self.command(mode),
            check=check,
            capture_output=True,
            text=True,
        )

    def generate(self) -> str:
        return self.run_command("generate").stdout.strip()

    def test_bundle_is_deterministic_license_complete_and_strictly_verifiable(
        self,
    ) -> None:
        first_digest = self.generate()
        first = {
            path: path.read_bytes()
            for path in (self.sbom, self.notices, self.size_report, self.candidate)
        }
        second_digest = self.generate()
        self.assertEqual(first_digest, second_digest)
        self.assertEqual(
            first,
            {
                path: path.read_bytes()
                for path in (self.sbom, self.notices, self.size_report, self.candidate)
            },
        )
        self.assertEqual(self.run_command("verify").stdout.strip(), first_digest)
        self.assertEqual(
            self.run_command("verify-bundle").stdout.strip(), first_digest
        )

        document = json.loads(self.sbom.read_bytes())
        cargo_components = [
            component
            for component in document["components"]
            if any(
                item["name"] == "ctx:dependency:ecosystem"
                for item in component.get("properties", [])
            )
        ]
        self.assertEqual(len(cargo_components), 9)
        self.assertTrue(
            all(component.get("licenses") for component in cargo_components)
        )
        tantivy = next(
            component
            for component in cargo_components
            if component["name"] == "tantivy"
        )
        tantivy_properties = {
            item["name"]: json.loads(item["value"])
            if item["value"].startswith(("[", "{"))
            else item["value"]
            for item in tantivy["properties"]
        }
        self.assertEqual(
            tantivy_properties["ctx:rust:resolved-crate-features"],
            list(TANTIVY_FEATURES),
        )

        candidate = json.loads(self.candidate.read_bytes())
        self.assertEqual(
            candidate["construction"],
            {
                "authority": "bazel-release-route-v1",
                "label": "//:ctx_release_linux_x64",
            },
        )
        self.assertEqual(
            candidate["tantivy"]["resolved_crate_features"],
            list(TANTIVY_FEATURES),
        )
        closure_names = {
            package["name"]
            for package in candidate["tantivy"]["dependency_closure"]
        }
        self.assertTrue(
            {"tantivy", "fs4", "lz4_flex", "memmap2", "tempfile", "zstd"}
            <= closure_names
        )
        self.assertIn("tantivy 0.26.1", self.notices.read_text(encoding="utf-8"))
        self.assertIn(
            "Synthetic license text for tantivy.",
            self.notices.read_text(encoding="utf-8"),
        )
        size = json.loads(self.size_report.read_bytes())
        self.assertEqual(size["artifact"]["size_bytes"], self.artifact.stat().st_size)

    def test_unselected_lock_package_is_not_reported(self) -> None:
        self.cargo_lock.write_text(
            self.cargo_lock.read_text(encoding="utf-8")
            + "\n"
            + self.package("target-only", "9.9.9", external=True)
            + "\n",
            encoding="utf-8",
        )
        self.write_build_info()
        self.generate()
        names = {
            component["name"]
            for component in json.loads(self.sbom.read_bytes())["components"]
        }
        self.assertNotIn("target-only", names)

    def test_missing_license_expression_is_rejected(self) -> None:
        manifest = (
            self.runfiles
            / "rules_rust~~crate~crates__tantivy-0.26.1/Cargo.toml"
        )
        manifest.write_text(
            manifest.read_text(encoding="utf-8").replace(
                'license = "MIT OR Apache-2.0"\n', ""
            ),
            encoding="utf-8",
        )
        rejected = self.run_command("generate", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("has no license expression", rejected.stderr)

    def test_tantivy_feature_drift_is_rejected(self) -> None:
        value = self.license_materials.read_text(encoding="utf-8")
        value = value.replace(
            "\tcolumnar-zstd-compression\n",
            "\tstopwords\n",
        )
        self.license_materials.write_text(
            "\n".join(sorted(value.splitlines())) + "\n",
            encoding="utf-8",
        )
        rejected = self.run_command("generate", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("configured Bazel Tantivy features", rejected.stderr)

    def test_substituted_artifact_or_evidence_is_rejected(self) -> None:
        self.generate()
        original_artifact = self.artifact.read_bytes()
        self.artifact.write_bytes(b"substituted artifact\n")
        rejected = self.run_command("verify-bundle", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("candidate manifest does not bind", rejected.stderr)

        self.artifact.write_bytes(original_artifact)
        candidate = json.loads(self.candidate.read_bytes())
        candidate["tantivy"]["dependency_closure"].pop()
        self.candidate.write_text(
            json.dumps(candidate, sort_keys=True, separators=(",", ":")) + "\n",
            encoding="utf-8",
        )
        rejected = self.run_command("verify-bundle", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("Tantivy contract is malformed", rejected.stderr)

        self.generate()
        self.notices.write_bytes(self.notices.read_bytes() + b"mutation\n")
        rejected = self.run_command("verify-bundle", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("does not bind third_party_notices", rejected.stderr)

    def test_windows_release_manifest_binds_sums_runtime_dll_and_authority(
        self,
    ) -> None:
        self.configure_windows_release()
        self.generate()
        authority = self.run_command("bind-release").stdout.strip()
        self.write_release_handoff()
        self.assertEqual(authority, self.bound_digest.read_text().strip())
        self.assertEqual(self.run_command("verify-release").stdout.strip(), authority)

        candidate = json.loads(self.bound_candidate.read_bytes())
        self.assertEqual(
            candidate["release_sums"],
            {
                "file": "SHA256SUMS",
                "sha256": hashlib.sha256(self.release_sums.read_bytes()).hexdigest(),
                "size_bytes": self.release_sums.stat().st_size,
            },
        )
        with zipfile.ZipFile(self.runtime) as archive:
            dll = archive.read("lib/onnxruntime.dll")
        self.assertEqual(
            candidate["runtime"],
            {
                "file": "ctx-onnxruntime-windows-x64.zip",
                "sha256": hashlib.sha256(self.runtime.read_bytes()).hexdigest(),
                "size_bytes": self.runtime.stat().st_size,
                "dll": {
                    "file": "lib/onnxruntime.dll",
                    "sha256": hashlib.sha256(dll).hexdigest(),
                    "size_bytes": len(dll),
                },
            },
        )

        handoff_sums = self.handoff / "SHA256SUMS"
        original_sums = handoff_sums.read_bytes()
        lines = original_sums.decode("ascii").splitlines()
        replacement = "0" if lines[0][0] != "0" else "1"
        lines[0] = replacement + lines[0][1:]
        handoff_sums.write_text("\n".join(lines) + "\n", encoding="ascii")
        rejected = self.run_command("verify-release", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("does not bind exact release SHA256SUMS", rejected.stderr)
        handoff_sums.write_bytes(original_sums)

        self.write_runtime(b"X" * len(dll))
        shutil.copyfile(
            self.runtime, self.handoff / "ctx-onnxruntime-windows-x64.zip"
        )
        rejected = self.run_command("verify-release", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("does not bind exact Windows runtime and DLL", rejected.stderr)

        # A complete caller-coordinated replacement remains unauthorized: even
        # canonical regenerated manifest/sums/archive/DLL records cannot change
        # the digest already committed by signed or attested release metadata.
        original_authority = authority
        self.write_release_sums()
        self.bound_candidate.unlink()
        self.bound_digest.unlink()
        self.run_command("bind-release")
        shutil.rmtree(self.handoff)
        self.write_release_handoff()
        self.expected_digest = original_authority
        rejected = self.run_command("verify-release", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("independently supplied expected digest", rejected.stderr)

    def test_verify_release_handoff_requires_exact_construction_names(self) -> None:
        self.configure_windows_release()
        self.generate()
        authority = self.run_command("bind-release").stdout.strip()
        self.write_release_handoff()

        self.assertTrue((self.handoff / "ctx.exe").is_file())
        self.assertFalse((self.handoff / "ctx-windows-x64.exe").exists())
        self.assertIn(
            "  ctx-windows-x64.exe\n",
            (self.handoff / "SHA256SUMS").read_text(encoding="ascii"),
        )
        self.assertEqual(self.run_command("verify-release").stdout.strip(), authority)

        (self.handoff / "ctx.exe").rename(
            self.handoff / "ctx-windows-x64.exe"
        )
        rejected = self.run_command("verify-release", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("exact production inventory", rejected.stderr)

    def test_verify_release_accepts_only_the_handoff_interface(self) -> None:
        self.configure_windows_release()
        self.generate()
        self.run_command("bind-release")
        self.write_release_handoff()
        command = self.command("verify-release")
        command.extend(("--artifact", str(self.artifact)))
        rejected = subprocess.run(command, capture_output=True, text=True, check=False)
        self.assertEqual(rejected.returncode, 2)
        self.assertIn("only through --handoff-dir", rejected.stderr)

    def test_windows_release_binding_requires_literal_outer_and_dll_names(self) -> None:
        self.configure_windows_release()
        self.generate()

        renamed_sums = self.root / "OTHER_SUMS"
        renamed_sums.write_bytes(self.release_sums.read_bytes())
        command = self.command("bind-release")
        command[command.index(str(self.release_sums))] = str(renamed_sums)
        rejected = subprocess.run(command, capture_output=True, text=True, check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("must be named SHA256SUMS", rejected.stderr)

        renamed_runtime = self.root / "other-runtime.zip"
        renamed_runtime.write_bytes(self.runtime.read_bytes())
        command = self.command("bind-release")
        command[command.index(str(self.runtime))] = str(renamed_runtime)
        rejected = subprocess.run(command, capture_output=True, text=True, check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("must be named ctx-onnxruntime-windows-x64.zip", rejected.stderr)

        self.write_runtime(dll_name="lib/other.dll")
        self.write_release_sums()
        rejected = self.run_command("bind-release", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("unexpected entry lib/other.dll", rejected.stderr)

    def test_windows_release_binding_requires_exact_runtime_and_sums_inventories(
        self,
    ) -> None:
        self.configure_windows_release()
        self.generate()

        self.write_runtime(omit="lib/vcruntime140_1.dll")
        self.write_release_sums()
        rejected = self.run_command("bind-release", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("do not exactly match the legacy sidecar layout", rejected.stderr)

        self.write_runtime(extra="lib/extra.dll")
        self.write_release_sums()
        rejected = self.run_command("bind-release", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("unexpected entry lib/extra.dll", rejected.stderr)

        self.write_runtime()
        self.write_release_sums()
        lines = self.release_sums.read_text(encoding="ascii").splitlines()
        self.release_sums.write_text(
            "\n".join(reversed(lines)) + "\n", encoding="ascii"
        )
        rejected = self.run_command("bind-release", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("exact canonical 24- or 34-entry", rejected.stderr)


if __name__ == "__main__":
    unittest.main()
