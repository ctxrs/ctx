#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "native-execution-proof.py"
spec = importlib.util.spec_from_file_location("native_execution_proof", MODULE_PATH)
assert spec is not None and spec.loader is not None
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

REQUIRED_SMOKE_STEPS = frozenset(
    {
        "version",
        "setup",
        "import",
        "search",
        "read_only",
        "released_defaults",
        "explicit_opt_outs",
        "semantic_offline_fail_closed",
    }
)


class NativeExecutionProofTest(unittest.TestCase):
    @staticmethod
    def passed_smoke() -> dict[str, object]:
        return {
            "kind": "ctx-native-candidate-smoke",
            "schema_version": 1,
            "status": "passed",
            "steps": {step: "passed" for step in REQUIRED_SMOKE_STEPS},
        }

    def test_required_smoke_steps_match_receipt_contract(self) -> None:
        self.assertEqual(module.REQUIRED_SMOKE_STEPS, REQUIRED_SMOKE_STEPS)

    def test_proof_binds_exact_artifact_and_passed_smoke(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / "ctx"
            smoke = root / "candidate-smoke.json"
            proof = root / "ctx-linux-x64.native-execution.json"
            artifact.write_bytes(b"candidate bytes")
            smoke.write_text(json.dumps(self.passed_smoke()) + "\n", encoding="utf-8")
            module.create("linux-x64", artifact, smoke, proof)
            module.verify("linux-x64", artifact, proof)
            artifact.write_bytes(b"changed candidate bytes")
            with self.assertRaisesRegex(ValueError, "different artifact"):
                module.verify("linux-x64", artifact, proof)

    def test_failed_smoke_cannot_create_proof(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / "ctx"
            smoke = root / "candidate-smoke.json"
            artifact.write_bytes(b"candidate bytes")
            smoke.write_text(
                '{"kind":"ctx-native-candidate-smoke","schema_version":1,"status":"failed"}\n',
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "not passed"):
                module.create("linux-x64", artifact, smoke, root / "proof.json")

    def test_every_platform_requires_complete_passed_smoke_steps(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / "ctx"
            smoke = root / "candidate-smoke.json"
            artifact.write_bytes(b"candidate bytes")
            for platform in module.PLATFORMS:
                for missing in REQUIRED_SMOKE_STEPS:
                    with self.subTest(platform=platform, missing=missing):
                        value = self.passed_smoke()
                        del value["steps"][missing]
                        smoke.write_text(json.dumps(value) + "\n", encoding="utf-8")
                        with self.assertRaisesRegex(ValueError, "missing required"):
                            module.create(platform, artifact, smoke, root / "proof.json")

if __name__ == "__main__":
    unittest.main()
