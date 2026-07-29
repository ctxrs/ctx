#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/dependency-advisory-gate.py"
FIXTURES = ROOT / "scripts/tests/fixtures/dependency-advisory"
FAKE_SCANNER = FIXTURES / "fake-osv-scanner.py"
NOW = "2026-07-29T17:00:00Z"


class AdvisoryGateTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        (self.repo / "Cargo.lock").write_text("fixture lock\n", encoding="utf-8")
        self.policy = self.repo / "policy.json"
        self.policy.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "scanner": {
                        "name": "osv-scanner",
                        "version": "2.4.0",
                        "sha256": hashlib.sha256(
                            FAKE_SCANNER.read_bytes()
                        ).hexdigest(),
                        "max_database_age_hours": 48,
                    },
                    "lockfiles": [
                        {
                            "path": "Cargo.lock",
                            "ecosystem": "crates.io",
                            "disposition": "scan",
                            "closure": "lockfile",
                            "role": "fixture release closure",
                        }
                    ],
                }
            ),
            encoding="utf-8",
        )
        self.exceptions = self.repo / "exceptions.json"
        self.write_exceptions([])
        self.database_root = self.root / "database"
        database = self.database_root / "osv-scanner/crates.io/all.zip"
        database.parent.mkdir(parents=True)
        database.write_bytes(b"fixture advisory database\n")
        self.metadata = self.root / "metadata.json"
        self.metadata.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "fetched_at": NOW,
                    "databases": [
                        {
                            "ecosystem": "crates.io",
                            "path": "osv-scanner/crates.io/all.zip",
                            "sha256": hashlib.sha256(database.read_bytes()).hexdigest(),
                            "size": database.stat().st_size,
                            "source_generation": "fixture",
                            "source_last_modified": "2026-07-29T16:00:00Z",
                        }
                    ],
                }
            ),
            encoding="utf-8",
        )
        self.receipt = self.root / "receipt.json"

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def write_exceptions(self, entries: list[dict[str, str]]) -> None:
        self.exceptions.write_text(
            json.dumps({"schema_version": 1, "exceptions": entries}),
            encoding="utf-8",
        )

    @staticmethod
    def exception(expires: str) -> dict[str, str]:
        return {
            "advisory_id": "RUSTSEC-2099-0001",
            "ecosystem": "crates.io",
            "package": "unsafe-crate",
            "version": "1.2.3",
            "lockfile": "Cargo.lock",
            "rationale": "Reviewed fixture risk is accepted for this bounded test.",
            "owner": "fixture-release-owner",
            "expires": expires,
        }

    def run_gate(
        self, fixture: str, scanner_exit: int = 0
    ) -> tuple[subprocess.CompletedProcess[str], dict[str, object]]:
        environment = os.environ.copy()
        environment["FAKE_OSV_FIXTURE"] = str(FIXTURES / fixture)
        environment["FAKE_OSV_EXIT"] = str(scanner_exit)
        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--repo-root",
                str(self.repo),
                "--policy",
                str(self.policy),
                "--exceptions",
                str(self.exceptions),
                "--database-root",
                str(self.database_root),
                "--database-metadata",
                str(self.metadata),
                "--scanner",
                str(FAKE_SCANNER),
                "--target-id",
                "fixture",
                "--output",
                str(self.receipt),
                "--now",
                NOW,
            ],
            text=True,
            capture_output=True,
            env=environment,
        )
        return result, json.loads(self.receipt.read_text(encoding="utf-8"))

    def test_clean(self) -> None:
        result, receipt = self.run_gate("osv-clean.json")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt["status"], "clean")
        self.assertFalse(receipt["coverage"]["os_packages_scanned"])

    def test_unreviewed_advisory(self) -> None:
        result, receipt = self.run_gate("osv-advisory.json", 1)
        self.assertEqual(result.returncode, 10)
        self.assertEqual(receipt["status"], "advisory")
        self.assertEqual(receipt["summary"]["unreviewed_advisory_count"], 1)

    def test_reviewed_exception(self) -> None:
        self.write_exceptions([self.exception("2026-07-30")])
        result, receipt = self.run_gate("osv-advisory.json", 1)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt["status"], "clean")
        self.assertEqual(receipt["summary"]["reviewed_exception_count"], 1)

    def test_expired_exception(self) -> None:
        self.write_exceptions([self.exception("2026-07-28")])
        result, receipt = self.run_gate("osv-advisory.json", 1)
        self.assertEqual(result.returncode, 11)
        self.assertEqual(receipt["status"], "expired_exception")

    def test_unknown_exception(self) -> None:
        self.write_exceptions([self.exception("2026-07-30")])
        result, receipt = self.run_gate("osv-clean.json")
        self.assertEqual(result.returncode, 12)
        self.assertEqual(receipt["status"], "unknown_exception")

    def test_tool_failure(self) -> None:
        result, receipt = self.run_gate("osv-clean.json", 7)
        self.assertEqual(result.returncode, 21)
        self.assertEqual(receipt["status"], "tool_failure")

    def test_scanner_digest_mismatch(self) -> None:
        policy = json.loads(self.policy.read_text(encoding="utf-8"))
        policy["scanner"]["sha256"] = "0" * 64
        self.policy.write_text(json.dumps(policy), encoding="utf-8")
        result, receipt = self.run_gate("osv-clean.json")
        self.assertEqual(result.returncode, 21)
        self.assertEqual(receipt["status"], "tool_failure")
        self.assertEqual(receipt["failure_reason"], "OSV-Scanner digest mismatch")

    def test_stale_database(self) -> None:
        metadata = json.loads(self.metadata.read_text(encoding="utf-8"))
        metadata["databases"][0]["source_last_modified"] = "2026-07-20T00:00:00Z"
        self.metadata.write_text(json.dumps(metadata), encoding="utf-8")
        result, receipt = self.run_gate("osv-clean.json")
        self.assertEqual(result.returncode, 20)
        self.assertEqual(receipt["status"], "stale_database")

    def test_unknown_lockfile(self) -> None:
        (self.repo / "package-lock.json").write_text("{}\n", encoding="utf-8")
        result, receipt = self.run_gate("osv-clean.json")
        self.assertEqual(result.returncode, 21)
        self.assertEqual(receipt["status"], "tool_failure")
        self.assertIn("unreviewed dependency lockfiles", receipt["failure_reason"])


if __name__ == "__main__":
    unittest.main()
