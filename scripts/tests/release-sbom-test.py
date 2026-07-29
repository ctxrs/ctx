#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


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
)
WORKSPACE_PACKAGES = (
    ("ctx", "crates/ctx-cli"),
    ("ctx-history-core", "crates/ctx-history-core"),
    ("ctx-history-index", "crates/ctx-history-index"),
    ("ctx-history-relational", "crates/ctx-history-relational"),
)
EXTERNAL_PACKAGES = (
    ("fs4", "0.1.0"),
    ("lz4_flex", "0.11.0"),
    ("memmap2", "0.9.0"),
    ("tantivy", "0.26.1"),
    ("tempfile", "3.0.0"),
    ("zstd", "0.13.0"),
)


class ReleaseSbomTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.runfiles = self.root / "runfiles"
        self.main_runfiles = self.runfiles / "_main"
        self.main_runfiles.mkdir(parents=True)

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
  "crates/ctx-history-relational",
]

[workspace.package]
version = "0.26.0"
license = "MIT"
repository = "https://github.com/ctxrs/ctx"

[workspace.dependencies]
tantivy = { version = "0.26.1", default-features = false, features = ["mmap", "lz4-compression", "columnar-zstd-compression"] }
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
            "@@//crates/ctx-history-relational:ctx_history_relational",
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
                    "ctx-history-relational",
                ),
            ),
            self.package("ctx-history-core", "0.26.0"),
            self.package(
                "ctx-history-index",
                "0.26.0",
                ("tantivy 0.26.1",),
            ),
            self.package("ctx-history-relational", "0.26.0"),
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

    def write_build_info(self) -> None:
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
                    "platform": "linux-x64",
                    "target": "x86_64-unknown-linux-gnu",
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

    def command(self, mode: str) -> list[str]:
        command = [
            sys.executable,
            str(SCRIPT),
            mode,
            "--artifact",
            str(self.artifact),
            "--build-info",
            str(self.build_info),
        ]
        if mode == "verify-bundle":
            return command + [
                "--sbom",
                str(self.sbom),
                "--notices",
                str(self.notices),
                "--size-report",
                str(self.size_report),
                "--candidate-manifest",
                str(self.candidate),
            ]
        command.extend(
            [
                "--product",
                "core",
                "--version",
                "0.26.0",
                "--target-id",
                "linux-x64",
                "--platform",
                "linux-x64",
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
        self.assertEqual(len(cargo_components), 10)
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


if __name__ == "__main__":
    unittest.main()
