#!/usr/bin/env python3
from __future__ import annotations

import ast
import datetime
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
import zipfile


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/release/run-with-release-advisory-inputs.py"
UPDATE_SCRIPT = ROOT / "scripts/update-release-advisory-db.py"
SPEC = importlib.util.spec_from_file_location("release_advisory_inputs", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
UPDATE_SPEC = importlib.util.spec_from_file_location(
    "release_advisory_database_update", UPDATE_SCRIPT
)
assert UPDATE_SPEC is not None and UPDATE_SPEC.loader is not None
UPDATE_MODULE = importlib.util.module_from_spec(UPDATE_SPEC)
UPDATE_SPEC.loader.exec_module(UPDATE_MODULE)


class Response(io.BytesIO):
    def __init__(self, payload: bytes, headers: dict[str, str] | None = None):
        super().__init__(payload)
        self.headers = headers or {}

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        self.close()


class ReleaseAdvisoryInputsTest(unittest.TestCase):
    @staticmethod
    def database_archive() -> bytes:
        payload = io.BytesIO()
        with zipfile.ZipFile(payload, "w") as archive:
            archive.writestr("GO-TEST-0001.json", "{}\n")
        return payload.getvalue()

    @staticmethod
    def metadata_response(generation: str, updated: str) -> Response:
        return Response(
            json.dumps({"generation": generation, "updated": updated}).encode()
        )

    def download_response(self, generation: str) -> Response:
        return Response(
            self.database_archive(),
            {"x-goog-generation": generation},
        )

    def update_database(
        self,
        responses: list[Response],
        ecosystems: tuple[str, ...] = ("crates.io",),
    ):
        database = self.root / "database"
        metadata = self.root / "metadata.json"
        argv = [
            str(UPDATE_SCRIPT),
            "--database-root",
            str(database),
            "--metadata",
            str(metadata),
        ]
        for ecosystem in ecosystems:
            argv.extend(["--ecosystem", ecosystem])
        with mock.patch.object(
            UPDATE_MODULE.urllib.request,
            "urlopen",
            side_effect=responses,
        ) as urlopen, mock.patch.object(
            sys,
            "argv",
            argv,
        ):
            result = UPDATE_MODULE.main()
        return result, metadata, urlopen

    def test_database_updater_uses_python_3_9_datetime_api(self) -> None:
        source = UPDATE_SCRIPT.read_text(encoding="utf-8")
        tree = ast.parse(
            source,
            filename=str(UPDATE_SCRIPT),
            feature_version=(3, 9),
        )
        datetime_imports = {
            alias.name
            for node in ast.walk(tree)
            if isinstance(node, ast.ImportFrom) and node.module == "datetime"
            for alias in node.names
        }
        self.assertNotIn("UTC", datetime_imports)
        self.assertIn("timezone", datetime_imports)

        namespace = {"__name__": "release_advisory_database_update_test"}
        exec(compile(tree, str(UPDATE_SCRIPT), "exec"), namespace)
        self.assertIs(namespace["UTC"], datetime.timezone.utc)

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        (self.root / "security").mkdir()
        (self.root / "scripts").mkdir()
        (self.root / "scripts/update-release-advisory-db.py").write_text(
            "raise SystemExit('test must intercept updater')\n",
            encoding="utf-8",
        )
        self.scanner_bytes = b"fixture scanner\n"
        self.scanner_sha256 = hashlib.sha256(self.scanner_bytes).hexdigest()
        (self.root / "security/release-advisory-policy-v1.json").write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "scanner": {
                        "name": "osv-scanner",
                        "version": "2.4.0",
                        "sha256_by_target": {
                            target: self.scanner_sha256
                            for target in MODULE.SCANNER_ASSETS
                        },
                    },
                }
            ),
            encoding="utf-8",
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def fake_run(self, argv, **kwargs):
        self.assertNotIn("BUILDKITE_API_ACCESS_TOKEN", kwargs["env"])
        database = Path(argv[argv.index("--database-root") + 1])
        metadata = Path(argv[argv.index("--metadata") + 1])
        database.mkdir(parents=True)
        metadata.write_text("{}\n", encoding="utf-8")
        return subprocess.CompletedProcess(argv, 0, "", "")

    def fake_prepared_inputs(self, *_args):
        scanner = self.root / "scanner"
        database = self.root / "database"
        metadata = self.root / "metadata.json"
        scanner.write_bytes(self.scanner_bytes)
        database.mkdir(exist_ok=True)
        metadata.write_text("{}\n", encoding="utf-8")
        return scanner, database, metadata, self.scanner_sha256

    def test_every_policy_target_has_an_exact_upstream_asset(self) -> None:
        self.assertEqual(
            MODULE.SCANNER_ASSETS,
            {
                "linux-x64": "osv-scanner_linux_amd64",
                "linux-arm64": "osv-scanner_linux_arm64",
                "macos-arm64": "osv-scanner_darwin_arm64",
                "macos-x64": "osv-scanner_darwin_amd64",
                "windows-x64": "osv-scanner_windows_amd64.exe",
            },
        )

    def test_loads_single_platform_policy_for_its_exact_target(self) -> None:
        policy = self.root / "single-platform-policy.json"
        policy.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "scanner": {
                        "name": "osv-scanner",
                        "version": "2.4.0",
                        "platform": "linux-x64",
                        "sha256": self.scanner_sha256,
                    },
                }
            ),
            encoding="utf-8",
        )
        self.assertEqual(
            MODULE.load_scanner_spec(policy, "linux-x64"),
            ("2.4.0", self.scanner_sha256, "osv-scanner_linux_amd64"),
        )

    def test_single_platform_policy_rejects_other_target(self) -> None:
        policy = self.root / "single-platform-policy.json"
        policy.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "scanner": {
                        "name": "osv-scanner",
                        "version": "2.4.0",
                        "platform": "linux-x64",
                        "sha256": self.scanner_sha256,
                    },
                }
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(MODULE.InputError, "does not cover target"):
            MODULE.load_scanner_spec(policy, "macos-x64")

    def test_prepares_checked_scanner_and_offline_database(self) -> None:
        task_root = self.root / "task"
        task_root.mkdir()
        with mock.patch.object(
            MODULE.urllib.request,
            "urlopen",
            return_value=Response(self.scanner_bytes),
        ) as urlopen, mock.patch.object(
            MODULE.subprocess,
            "run",
            side_effect=self.fake_run,
        ):
            scanner, database, metadata, digest = MODULE.prepare_inputs(
                self.root,
                task_root,
                "linux-arm64",
            )
        self.assertEqual(scanner.read_bytes(), self.scanner_bytes)
        self.assertTrue(os.access(scanner, os.X_OK))
        self.assertTrue(database.is_dir())
        self.assertTrue(metadata.is_file())
        self.assertEqual(digest, self.scanner_sha256)
        self.assertEqual(
            urlopen.call_args.args[0].full_url,
            "https://github.com/google/osv-scanner/releases/download/"
            "v2.4.0/osv-scanner_linux_arm64",
        )

    def test_prepares_every_requested_database_ecosystem_once(self) -> None:
        task_root = self.root / "mixed-task"
        task_root.mkdir()
        observed_command = []

        def fake_run(argv, **kwargs):
            observed_command.extend(argv)
            return self.fake_run(argv, **kwargs)

        with mock.patch.object(
            MODULE.urllib.request,
            "urlopen",
            return_value=Response(self.scanner_bytes),
        ), mock.patch.object(
            MODULE.subprocess,
            "run",
            side_effect=fake_run,
        ):
            MODULE.prepare_inputs(
                self.root,
                task_root,
                "linux-x64",
                ("npm", "crates.io", "npm"),
            )

        ecosystem_arguments = [
            observed_command[index + 1]
            for index, value in enumerate(observed_command)
            if value == "--ecosystem"
        ]
        self.assertEqual(ecosystem_arguments, ["crates.io", "npm"])

    def test_rejects_an_empty_database_ecosystem_set(self) -> None:
        task_root = self.root / "empty-task"
        task_root.mkdir()
        with mock.patch.object(
            MODULE.urllib.request,
            "urlopen",
            return_value=Response(self.scanner_bytes),
        ), mock.patch.object(MODULE.subprocess, "run") as run:
            with self.assertRaisesRegex(MODULE.InputError, "ecosystem is invalid"):
                MODULE.prepare_inputs(self.root, task_root, "linux-x64", ())
        run.assert_not_called()

    def test_cached_ordinary_endpoint_cannot_certify_latest(self) -> None:
        with mock.patch.object(
            UPDATE_MODULE.urllib.request,
            "urlopen",
            return_value=self.metadata_response("101", "2020-01-01T00:00:00Z"),
        ) as urlopen:
            generation, _modified = UPDATE_MODULE.latest_source_metadata("crates.io")
        request = urlopen.call_args.args[0]
        self.assertEqual(generation, "101")
        self.assertEqual(request.full_url, UPDATE_MODULE.METADATA_SOURCES["crates.io"])
        self.assertNotEqual(request.full_url, UPDATE_MODULE.SOURCES["crates.io"])
        self.assertEqual(request.get_header("Cache-control"), "no-cache")

    def test_two_ecosystem_seal_rejects_one_superseded_generation(self) -> None:
        metadata = self.root / "metadata.json"
        metadata.write_text(
            "preexisting metadata must be invalidated\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(SystemExit, "no longer current: npm"):
            self.update_database(
                [
                    self.metadata_response("100", "2020-01-01T00:00:00Z"),
                    self.metadata_response("200", "2020-01-01T00:00:00Z"),
                    self.download_response("100"),
                    self.download_response("200"),
                    self.metadata_response("100", "2020-01-01T00:00:00Z"),
                    self.metadata_response("201", "2020-01-02T00:00:00Z"),
                ],
                ("crates.io", "npm"),
            )
        self.assertFalse(metadata.exists())

    def test_exact_generation_download_preserves_canonical_origin(self) -> None:
        result, metadata, urlopen = self.update_database(
            [
                self.metadata_response("100", "2020-01-01T00:00:00Z"),
                self.download_response("100"),
                self.metadata_response("100", "2020-01-01T00:00:00Z"),
            ]
        )
        self.assertEqual(result, 0)
        sealed = json.loads(metadata.read_text(encoding="utf-8"))
        self.assertEqual(sealed["schema_version"], 2)
        self.assertEqual(sealed["databases"][0]["source_generation"], "100")
        self.assertEqual(
            sealed["databases"][0]["source_last_modified"],
            "2020-01-01T00:00:00Z",
        )
        self.assertEqual(
            sealed["databases"][0]["source_url"],
            UPDATE_MODULE.SOURCES["crates.io"],
        )
        self.assertEqual(
            urlopen.call_args_list[1].args[0].full_url,
            f"{UPDATE_MODULE.SOURCES['crates.io']}?generation=100",
        )
        self.assertEqual(
            urlopen.call_args_list[2].args[0].full_url,
            UPDATE_MODULE.METADATA_SOURCES["crates.io"],
        )

    def test_rejects_scanner_before_database_update_on_digest_mismatch(self) -> None:
        task_root = self.root / "bad-task"
        task_root.mkdir()
        with mock.patch.object(
            MODULE.urllib.request,
            "urlopen",
            return_value=Response(b"tampered scanner\n"),
        ), mock.patch.object(MODULE.subprocess, "run") as run:
            with self.assertRaisesRegex(MODULE.InputError, "digest does not match"):
                MODULE.prepare_inputs(self.root, task_root, "macos-x64")
        run.assert_not_called()
        self.assertFalse((task_root / "scanner/osv-scanner").exists())

    def test_wrapped_release_command_retains_full_environment(self) -> None:
        release_environment = {
            "APPLE_SIGNING_IDENTITY": "sentinel-signing-secret",
            "BUILDKITE_AGENT_ACCESS_TOKEN": "sentinel-buildkite-secret",
            "NOTARYTOOL_PASSWORD": "sentinel-notary-secret",
        }
        observed_environment = {}

        def fake_release_run(argv, **kwargs):
            self.assertEqual(argv, ["release-command"])
            observed_environment.update(kwargs["env"])
            return subprocess.CompletedProcess(argv, 0)

        with mock.patch.object(
            MODULE,
            "ROOT",
            self.root,
        ), mock.patch.object(
            MODULE,
            "prepare_inputs",
            side_effect=self.fake_prepared_inputs,
        ), mock.patch.object(
            MODULE.subprocess,
            "run",
            side_effect=fake_release_run,
        ), mock.patch.object(
            MODULE.os,
            "environ",
            release_environment,
        ), mock.patch.object(
            MODULE.sys,
            "argv",
            [str(SCRIPT), "--target", "linux-x64", "--", "release-command"],
        ):
            self.assertEqual(MODULE.main(), 0)

        for name, value in release_environment.items():
            self.assertEqual(observed_environment[name], value)
        self.assertEqual(observed_environment["CTX_OSV_SCANNER"], str(self.root / "scanner"))
        self.assertEqual(
            observed_environment["CTX_OSV_DATABASE_DIR"],
            str(self.root / "database"),
        )
        self.assertEqual(
            observed_environment["CTX_OSV_DATABASE_METADATA"],
            str(self.root / "metadata.json"),
        )


if __name__ == "__main__":
    unittest.main()
