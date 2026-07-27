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


class ReleaseSbomTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.artifact = self.root / "ctx"
        self.artifact.write_bytes(b"exact release artifact\n")
        self.cargo_lock = self.root / "Cargo.lock"
        self.cargo_lock.write_text(
            """\
version = 4

[[package]]
name = "ctx"
version = "0.26.0"
dependencies = [
 "dependency 1.2.3 (registry+https://github.com/rust-lang/crates.io-index)",
]

[[package]]
name = "dependency"
version = "1.2.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
""",
            encoding="utf-8",
        )
        self.module_file = self.root / "MODULE.bazel"
        self.module_file.write_text(
            'module(name = "ctx")\nbazel_dep(name = "rules_python", version = "1.4.1")\n',
            encoding="utf-8",
        )
        self.module_lock = self.root / "MODULE.bazel.lock"
        self.module_lock.write_text('{"lockFileVersion":21}\n', encoding="utf-8")
        self.target_inventory = self.root / "target-dependency-inventory.txt"
        self.target_inventory.write_text(
            "//crates/ctx-cli:ctx\n"
            "@@rules_rust~~crate~crates__dependency-1.2.3//:dependency\n",
            encoding="utf-8",
        )
        self.build_info = self.root / "ctx.build-info.json"
        cargo_sha = hashlib.sha256(self.cargo_lock.read_bytes()).hexdigest()
        artifact_sha = hashlib.sha256(self.artifact.read_bytes()).hexdigest()
        self.build_info.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "artifact_sha256": artifact_sha,
                    "cargo_lock_sha256": cargo_sha,
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
        self.sbom = self.root / "ctx.cdx.json"

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def command(self, mode: str, **paths: Path) -> list[str]:
        command = [
            sys.executable,
            str(SCRIPT),
            mode,
            "--product",
            "core",
            "--version",
            "0.26.0",
            "--platform",
            "linux-x64",
            "--artifact",
            str(self.artifact),
            "--build-info",
            str(self.build_info),
            "--cargo-lock",
            str(self.cargo_lock),
            "--module-lock",
            str(self.module_lock),
            "--module-file",
            str(self.module_file),
            "--target-inventory",
            str(self.target_inventory),
        ]
        for name, path in paths.items():
            command.extend((f"--{name.replace('_', '-')}", str(path)))
        return command

    def test_generation_is_deterministic_and_strictly_verifiable(self) -> None:
        first = subprocess.run(
            self.command("generate", output=self.sbom),
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        first_bytes = self.sbom.read_bytes()
        second = subprocess.run(
            self.command("generate", output=self.sbom),
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        self.assertEqual(first_bytes, self.sbom.read_bytes())
        self.assertEqual(first, second)
        verified = subprocess.run(
            self.command("verify", sbom=self.sbom),
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        self.assertEqual(verified, first)

        document = json.loads(first_bytes)
        self.assertEqual(document["bomFormat"], "CycloneDX")
        self.assertEqual(document["specVersion"], "1.6")
        self.assertNotIn("timestamp", document["metadata"])
        self.assertNotIn("serialNumber", document)
        properties = {
            item["name"]: item["value"] for item in document["metadata"]["properties"]
        }
        self.assertEqual(
            properties["ctx:build-info:classification"],
            "sanitized-release-evidence-not-slsa-provenance",
        )
        self.assertEqual(properties["ctx:source:public-commit"], COMMIT)
        self.assertEqual(
            document["metadata"]["component"]["hashes"][0]["content"],
            hashlib.sha256(self.artifact.read_bytes()).hexdigest(),
        )
        self.assertEqual(
            sorted(component["name"] for component in document["components"]),
            [
                "Cargo.lock",
                "MODULE.bazel.lock",
                "ctx",
                "dependency",
                "target-dependency-inventory.txt",
            ],
        )

    def test_inventory_omits_unselected_lock_material(self) -> None:
        self.cargo_lock.write_text(
            self.cargo_lock.read_text(encoding="utf-8")
            + """\

[[package]]
name = "other-target-only"
version = "9.9.9"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
""",
            encoding="utf-8",
        )
        cargo_sha = hashlib.sha256(self.cargo_lock.read_bytes()).hexdigest()
        value = json.loads(self.build_info.read_text(encoding="utf-8"))
        value["cargo_lock_sha256"] = cargo_sha
        self.build_info.write_text(
            json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n",
            encoding="utf-8",
        )
        subprocess.run(
            self.command("generate", output=self.sbom),
            check=True,
            capture_output=True,
        )
        names = {
            component["name"]
            for component in json.loads(self.sbom.read_bytes())["components"]
        }
        self.assertNotIn("other-target-only", names)

    def test_inventory_missing_selected_dependency_is_rejected(self) -> None:
        self.target_inventory.write_text(
            "//crates/ctx-cli:ctx\n",
            encoding="utf-8",
        )
        rejected = subprocess.run(
            self.command("generate", output=self.sbom),
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("inventory omits a dependency", rejected.stderr)

    def test_tampered_artifact_lock_or_sbom_is_rejected(self) -> None:
        subprocess.run(
            self.command("generate", output=self.sbom),
            check=True,
            capture_output=True,
        )
        original_artifact = self.artifact.read_bytes()
        self.artifact.write_bytes(b"substituted artifact\n")
        rejected = subprocess.run(
            self.command("verify", sbom=self.sbom), capture_output=True, text=True
        )
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("build-info does not bind", rejected.stderr)

        self.artifact.write_bytes(original_artifact)
        original_lock = self.cargo_lock.read_bytes()
        self.cargo_lock.write_bytes(original_lock + b"\n# mutation\n")
        rejected = subprocess.run(
            self.command("verify", sbom=self.sbom), capture_output=True, text=True
        )
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("build-info does not bind", rejected.stderr)

        self.cargo_lock.write_bytes(original_lock)
        value = json.loads(self.sbom.read_bytes())
        value["components"] = []
        self.sbom.write_text(json.dumps(value) + "\n", encoding="utf-8")
        rejected = subprocess.run(
            self.command("verify", sbom=self.sbom), capture_output=True, text=True
        )
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("does not match the exact artifact", rejected.stderr)

    def test_symlink_sbom_is_rejected(self) -> None:
        target = self.root / "target.cdx.json"
        subprocess.run(
            self.command("generate", output=target),
            check=True,
            capture_output=True,
        )
        self.sbom.symlink_to(target)
        rejected = subprocess.run(
            self.command("verify", sbom=self.sbom), capture_output=True, text=True
        )
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("not a regular file", rejected.stderr)


if __name__ == "__main__":
    unittest.main()
