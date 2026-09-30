#!/usr/bin/env python3
"""Offline runner regressions, not evidence of released-binary compatibility."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import tomllib
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("upgrade_walk", ROOT / "scripts/release/upgrade-walk.py")
WALK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WALK)
VERSIONS = ["1.6.3", "1.6.4", "2.0.4"]
# Literal marker spellings from cli-install-shell-platform.js, also used by the native updater.
MARKER_PLATFORMS = {
    "linux_x64": "linux-x64",
    "linux_aarch64": "linux-aarch64",
    "macos_x64": "macos-x64",
    "macos_arm64": "macos-arm64",
}


class UpgradeWalkTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.walk = WALK.Walk(self.root, VERSIONS, 1)
        self.commands = []
        self.current = None
        self.updates = iter(VERSIONS[1:])
        self.tamper = False
        self.missing_history = False
        self.terminal_status = "up_to_date"
        self.stranded = False
        self.changed_citations = False
        self.daemon_running = False
        self.man_receipt = {"schema_version": 1, "status": "disabled"}

    def release(self, version):
        return {
            "url": f"{WALK.METADATA_BASE}/{version}/ctx-release-metadata.env",
            "sha256": hashlib.sha256(version.encode()).hexdigest(),
            "metadata_sha256": "offline-unit-stub",
        }

    def command(self, label, args, env=None):
        self.walk.report["stage"] = label
        environment = self.walk.env if env is None else env
        self.commands.append((args, environment.copy()))
        # Model only the harness's supported controls; the released parser rejects unknown keys.
        config = Path(self.walk.env["CTX_DATA_ROOT"]) / "config.toml"
        supported = {"analytics.enabled", "upgrade.auto", "search.semantic",
                     "daemon.enabled", "daemon.mode", "indexing.mode"}
        for section, values in tomllib.loads(config.read_text()).items():
            for key in values:
                if f"{section}.{key}" not in supported:
                    raise RuntimeError(f"unknown config key in offline fixture: {section}.{key}")
        if args[0] == "sh":
            bootstrap = "CTX_RELEASE_METADATA_URL" in environment
            self.current = self.walk.versions[0] if bootstrap else self.walk.versions[-1]
            if not bootstrap:
                self.assertTrue(self.walk.installer_recovery)
                text = config.read_text().replace('[daemon]\nenabled = false\n', '[daemon]\n')
                if '[indexing]' not in text:
                    text += '[indexing]\nmode = "manual"\n'
                config.write_text(text)
            self.walk.binary.parent.mkdir(parents=True, exist_ok=True)
            if label != "installer-repeat":
                self.walk.binary.write_text("wrong bytes" if not bootstrap and self.tamper else self.current)
                marker = {"schema_version": 1, "manager": "ctx-hosted-installer", "channel": "stable",
                          "install_path": str(self.walk.binary), "platform": MARKER_PLATFORMS[self.walk.platform],
                          "version": self.current, "sha256": self.release(self.current)["sha256"],
                          "install_attempt_id": "ia_bootstrap" if bootstrap else "ia_recovery",
                          "installed_at": "2026-01-01T00:00:00Z", "source_commit": "a" * 40,
                          "metadata_url": self.release(self.current)["url"],
                          "artifact_url": f"https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/stable/{self.current}/ctx"}
                if self.man_receipt is not None:
                    marker["man_pages"] = self.man_receipt
            else:
                marker = json.loads(self.walk.marker.read_text())
            # A repeat may bind the same ownership bytes at a content-addressed path.
            ownership = b"CTX_INSTALL_INTEGRATIONS_V1\nrecords_sha256\t" + hashlib.sha256(b"").hexdigest().encode() + b"\n"
            checksum = hashlib.sha256(ownership).hexdigest()
            ownership_path = Path(str(self.walk.binary) + ".install-integrations"
                                  + ("." + checksum if label == "installer-repeat" else ""))
            ownership_path.write_bytes(ownership)
            marker.update(integrations_path=str(ownership_path), integrations_sha256=checksum)
            self.walk.marker.write_text(json.dumps(marker))
            return ""
        self.assertEqual(args[0], str(self.walk.binary))
        self.assertNotIn("CTX_RELEASE_METADATA_URL", environment)
        self.assertEqual(environment["CTX_DAEMON_ENABLED"], "false")
        if args[1] == "--version":
            return f"ctx {self.current}\n"
        if args[1] == "upgrade":
            self.assertEqual(args[1:], ["upgrade", "--format", "json"])
            if self.stranded:
                return json.dumps({
                    "schema_version": 1, "command": "upgrade", "ok": True,
                    "channel": "stable", "current_version": "2.0.2", "latest_version": "1.6.5",
                    "applied": False, "update_available": False, "update_was_available": False,
                    "status": "up_to_date", "managed": True, "dry_run": False,
                    "metadata_url": "https://cli.ctx.rs/functions/v2/releases/stable/ctx-release-metadata.env",
                    "artifact_url": "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/stable/1.6.5/ctx",
                    "install_path": str(self.walk.binary), "platform": MARKER_PLATFORMS[self.walk.platform],
                    "upgrade_attempt_id": "ua_synthetic_discovery", "warnings": [],
                    "message": "ctx 2.0.2 is already installed.",
                })
            version = next(self.updates, None)
            if isinstance(version, Exception):
                raise version
            if version:
                self.current = version
                self.walk.binary.write_text("wrong bytes" if self.tamper else version)
            return json.dumps({"ok": True, "applied": version is not None,
                               "update_available": False,
                               "status": "upgraded" if version else self.terminal_status})
        if args[1] == "import":
            self.assertEqual(environment["CTX_DAEMON_AUTOSTART_OFF"], "0")
            self.assertNotIn("--no-daemon", args)
            return "{}"
        if args[1] == "daemon":
            if args[2] == "status":
                return json.dumps({"running": False, "daemon": {"enabled": False,
                                                               "running": self.daemon_running}})
            self.assertEqual(args[1:], ["daemon", "disable", "--format", "json"])
            self.assertNotEqual(Path(environment["CTX_DATA_ROOT"]), Path(environment["HOME"]) / ".ctx")
            return '{"running":false}'
        if args[1] == "search":
            self.assertEqual(args[-4:], ["--refresh", "off", "--format", "json"])
            return json.dumps({"retrieval": {"requested_mode": "lexical", "effective_mode": "lexical"},
                               "results": [] if self.missing_history else [{
                                   "snippet": "Add a parser test.",
                                   "citations": [{"provider": "custom", "session_id": "synthetic-session",
                                                  "item_id": "changed" if self.changed_citations
                                                  and self.current != self.walk.versions[0] else "synthetic-event"}],
                               }]})
        self.fail(f"unexpected command: {args}")

    def execute_offline(self):
        with mock.patch.object(self.walk, "run", side_effect=self.command), \
                mock.patch.object(self.walk, "release", side_effect=self.release), \
                mock.patch.object(self.walk, "download", side_effect=lambda label, url, path: path.write_text("installer")):
            self.walk.execute()

    def test_one_install_real_updater_calls_each_hop_and_terminal(self):
        self.execute_offline()
        self.assertEqual(self.walk.report["status"], "passed")
        installs = [(args, env) for args, env in self.commands if args[0] == "sh"]
        self.assertEqual(len(installs), 1)
        args, env = installs[0]
        self.assertEqual(args[2:], ["--no-setup", "--no-skill", "--no-man", "--no-modify-path"])
        self.assertEqual(env["CTX_RELEASE_METADATA_URL"], self.release("1.6.3")["url"])
        self.assertEqual(sum(args[1] == "upgrade" for args, _ in self.commands), 3)
        self.assertEqual(sum(args[1] == "import" for args, _ in self.commands), 1)
        self.assertEqual(sum(args[1] == "daemon" for args, _ in self.commands), 1)
        self.assertEqual(sum(args[1] == "search" for args, _ in self.commands), 4)
        self.assertEqual([item["version"] for item in self.walk.report["snapshots"]], VERSIONS + ["2.0.4"])
        self.assertFalse(self.walk.report["automatic_cadence_tested"])

    def recovery_walk(self, installer=None):
        self.walk = WALK.Walk(self.root, ["2.0.2", "2.2.0"], 1,
                              installer_recovery=True, installer=installer)
        self.stranded = True

    def test_ordinary_walk_can_start_at_2_0_2_without_reinstall(self):
        self.walk = WALK.Walk(self.root, ["2.0.2", "2.2.1"], 1)
        self.updates = iter(["2.2.1"])
        self.execute_offline()
        self.assertEqual(self.walk.report["kind"], "manual-released-upgrade-walk")
        self.assertEqual(self.walk.report["status"], "passed")
        self.assertEqual(self.walk.report["observed_versions"], ["ctx 2.0.2", "ctx 2.2.1", "ctx 2.2.1"])
        installs = [env for args, env in self.commands if args[0] == "sh"]
        self.assertEqual(len(installs), 1)
        self.assertEqual(installs[0]["CTX_RELEASE_METADATA_URL"], self.release("2.0.2")["url"])

    def test_stranded_ordinary_walk_is_not_implicitly_recovered(self):
        self.walk = WALK.Walk(self.root, ["2.0.2", "2.2.0"], 1)
        self.stranded = True
        with self.assertRaisesRegex(RuntimeError, "expected ctx 2.2.0, got ctx 2.0.2"):
            self.execute_offline()
        self.assertEqual(sum(args[0] == "sh" for args, _ in self.commands), 1)
        self.assertEqual(self.walk.report["status"], "failed")

    def test_recovery_records_wrong_target_then_verifies_reinstall_and_repeat(self):
        self.recovery_walk()
        self.execute_offline()
        report = self.walk.report
        self.assertEqual(report["kind"], "released-installer-recovery")
        self.assertEqual(report["status"], "passed")
        self.assertFalse(report["automatic_cadence_tested"])
        self.assertEqual(report["discovery_receipt"]["latest_version"], "1.6.5")
        self.assertFalse(report["discovery_receipt"]["applied"])
        self.assertEqual([item["version"] for item in report["snapshots"]],
                         ["2.0.2", "2.0.2", "2.2.0", "2.2.0"])
        installs = [(args, env) for args, env in self.commands if args[0] == "sh"]
        self.assertEqual(len(installs), 3)
        for args, env in installs[1:]:
            self.assertEqual(args[2:], ["--no-setup", "--no-skill", "--no-man", "--no-modify-path"])
            self.assertNotIn("CTX_RELEASE_METADATA_URL", env)
            self.assertNotIn("CTX_RELEASE_METADATA_SIGNATURE_URL", env)
        self.assertEqual(sum(args[1] == "upgrade" for args, _ in self.commands), 1)
        self.assertEqual(report["recovery_installer"]["source"], "hosted")
        self.assertEqual(report["recovery_installer"]["sha256"], hashlib.sha256(b"installer").hexdigest())
        self.assertEqual([item["indexing"] for item in report["preferences"]], ["manual"] * 3)
        before, recovered, repeated = report["installation_witnesses"]
        self.assertEqual((recovered["device"], recovered["inode"]), (repeated["device"], repeated["inode"]))
        self.assertEqual(before["marker"]["install_attempt_id"], "ia_bootstrap")
        self.assertEqual(recovered["marker"]["install_attempt_id"], "ia_recovery")
        self.assertEqual(repeated["marker"]["install_attempt_id"], "ia_recovery")
        self.assertNotEqual(recovered["marker"]["integrations_path"], repeated["marker"]["integrations_path"])
        self.assertEqual(recovered["marker"]["integrations_sha256"], repeated["marker"]["integrations_sha256"])
        config = (Path(self.walk.env["CTX_DATA_ROOT"]) / "config.toml").read_text()
        self.assertIn('[indexing]\nmode = "manual"', config)
        self.assertNotIn('[daemon]\nenabled', config)

    def test_recovery_retains_absent_man_receipt(self):
        self.recovery_walk()
        self.man_receipt = None
        self.execute_offline()
        self.assertEqual(self.walk.report["status"], "passed")
        for witness in self.walk.report["installation_witnesses"]:
            self.assertNotIn("man_pages", witness["marker"])

    def test_offline_fixture_rejects_unknown_config_before_bootstrap(self):
        self.recovery_walk()
        config = Path(self.walk.env["CTX_DATA_ROOT"]) / "config.toml"
        config.write_text(config.read_text() + '[unsupported]\nsetting = "invalid"\n')
        with self.assertRaisesRegex(RuntimeError, "unknown config key.*unsupported.setting"):
            self.execute_offline()
        self.assertEqual(self.walk.report["stage"], "install-2.0.2")
        self.assertFalse(self.walk.binary.exists())
        self.assertEqual(self.walk.report["snapshots"], [])

    def test_marker_platforms_match_installer_schema(self):
        for metadata_key, marker_tag in MARKER_PLATFORMS.items():
            with self.subTest(platform=marker_tag):
                self.recovery_walk()
                self.walk.platform = metadata_key
                self.execute_offline()
                self.assertEqual(self.walk.report["status"], "passed")
                for witness in self.walk.report["installation_witnesses"]:
                    self.assertEqual(witness["marker"]["platform"], marker_tag)
                marker = json.loads(self.walk.marker.read_text())
                other_tag = "macos-arm64" if marker_tag == "linux-x64" else "linux-x64"
                for invalid in (metadata_key, other_tag):
                    marker["platform"] = invalid
                    self.walk.marker.write_text(json.dumps(marker))
                    with self.assertRaisesRegex(RuntimeError, "marker differs from installed identity"):
                        self.walk.installation_witness("wrong-platform", "2.2.0",
                                                       self.release("2.2.0")["sha256"])

    def test_repeat_detects_replacement_with_identical_executable_bytes(self):
        self.recovery_walk()
        original = self.command

        def command(label, args, env=None):
            result = original(label, args, env)
            if label == "installer-repeat":
                replacement = self.root / "replacement"
                replacement.write_bytes(self.walk.binary.read_bytes())
                replacement.replace(self.walk.binary)
            return result

        with mock.patch.object(self, "command", side_effect=command):
            with self.assertRaisesRegex(RuntimeError, "replaced the executable device/inode"):
                self.execute_offline()
        self.assertEqual(WALK.sha256(self.walk.binary), self.release("2.2.0")["sha256"])

    def test_repeat_detects_changed_marker_identity_and_attribution(self):
        for field, value in (("install_attempt_id", "ia_another_attempt"),
                             ("installed_at", "2026-01-02T00:00:00Z"), ("source_commit", "b" * 40)):
            with self.subTest(field=field):
                self.recovery_walk()
                original = self.command

                def command(label, args, env=None):
                    result = original(label, args, env)
                    if label == "installer-repeat":
                        marker = json.loads(self.walk.marker.read_text())
                        marker[field] = value
                        self.walk.marker.write_text(json.dumps(marker))
                    return result

                with mock.patch.object(self, "command", side_effect=command):
                    with self.assertRaisesRegex(RuntimeError, "changed stable marker identity or attribution"):
                        self.execute_offline()

    def test_recovery_detects_marker_disagreeing_with_installed_bytes(self):
        self.recovery_walk()
        original = self.command

        def command(label, args, env=None):
            result = original(label, args, env)
            if label == "installer-recovery":
                marker = json.loads(self.walk.marker.read_text())
                marker["sha256"] = self.release("2.0.2")["sha256"]
                self.walk.marker.write_text(json.dumps(marker))
            return result

        with mock.patch.object(self, "command", side_effect=command):
            with self.assertRaisesRegex(RuntimeError, "marker differs from installed identity"):
                self.execute_offline()

    def test_recovery_detects_enabled_missing_or_invented_man_receipt(self):
        disabled = {"schema_version": 1, "status": "disabled"}
        for before, after in ((disabled, {"schema_version": 1, "status": "installed"}),
                              (disabled, None), (None, disabled)):
            with self.subTest(before=before, after=after):
                self.recovery_walk()
                self.man_receipt = before
                original = self.command

                def command(label, args, env=None):
                    result = original(label, args, env)
                    if label == "installer-recovery":
                        marker = json.loads(self.walk.marker.read_text())
                        marker.pop("man_pages", None)
                        if after is not None:
                            marker["man_pages"] = after
                        self.walk.marker.write_text(json.dumps(marker))
                    return result

                with mock.patch.object(self, "command", side_effect=command):
                    with self.assertRaisesRegex(RuntimeError, "man receipt"):
                        self.execute_offline()

    def test_repeat_rejects_corrupt_integration_augmentation(self):
        for mutation in ("corrupt", "remove"):
            with self.subTest(mutation=mutation):
                self.recovery_walk()
                original = self.command

                def command(label, args, env=None):
                    result = original(label, args, env)
                    if label == "installer-repeat":
                        marker = json.loads(self.walk.marker.read_text())
                        if mutation == "corrupt":
                            Path(marker["integrations_path"]).write_text("changed ownership bytes")
                        else:
                            del marker["integrations_path"], marker["integrations_sha256"]
                            self.walk.marker.write_text(json.dumps(marker))
                    return result

                with mock.patch.object(self, "command", side_effect=command):
                    with self.assertRaisesRegex(RuntimeError, "integration ownership"):
                        self.execute_offline()

    def test_local_installer_only_replaces_recovery_script_and_binds_its_bytes(self):
        local = self.root / "reviewed.sh"
        local.write_text("reviewed installer bytes")
        self.recovery_walk(local)
        self.execute_offline()
        installs = [args[1] for args, _ in self.commands if args[0] == "sh"]
        self.assertEqual(installs, [str(self.root / "install.sh")]
                         + [str(self.root / "recovery-installer.sh")] * 2)
        self.assertEqual(self.walk.report["recovery_installer"],
                         {"source": "local", "sha256": hashlib.sha256(local.read_bytes()).hexdigest()})

    def test_recovery_does_not_hide_a_changed_feed(self):
        self.recovery_walk()
        self.stranded = False
        self.updates = iter(["2.2.0"])
        with self.assertRaisesRegex(RuntimeError, "requires a stranded v2 discovery receipt"):
            self.execute_offline()
        self.assertTrue(self.walk.report["discovery_receipt"]["applied"])
        self.assertEqual(sum(args[0] == "sh" for args, _ in self.commands), 1)

    def test_recovery_installer_failure_is_not_retried_as_success(self):
        self.recovery_walk()
        original = self.command

        def command(label, args, env=None):
            if label == "installer-recovery":
                raise RuntimeError("installer lifecycle proof failed")
            return original(label, args, env)

        with mock.patch.object(self, "command", side_effect=command):
            with self.assertRaisesRegex(RuntimeError, "installer lifecycle proof failed"):
                self.execute_offline()
        self.assertEqual(self.walk.report["status"], "failed")
        self.assertEqual(self.walk.report["discovery_receipt"]["latest_version"], "1.6.5")
        self.assertEqual(self.walk.binary.read_text(), "2.0.2")

    def test_recovery_rejects_wrong_bytes_lost_citations_and_running_daemon(self):
        for setting, message in (("tamper", "checksum differs"),
                                 ("changed_citations", "changed history citations"),
                                 ("daemon_running", "enabled or started")):
            with self.subTest(setting=setting):
                self.recovery_walk()
                setattr(self, setting, True)
                with self.assertRaisesRegex(RuntimeError, message):
                    self.execute_offline()
                setattr(self, setting, False)

    def test_persisted_opt_in_is_not_hidden_by_disabled_environment(self):
        self.recovery_walk()
        original = self.command

        def command(label, args, env=None):
            result = original(label, args, env)
            if label == "installer-recovery":
                config = Path(self.walk.env["CTX_DATA_ROOT"]) / "config.toml"
                config.write_text(config.read_text().replace('[analytics]\nenabled = false',
                                                             '[analytics]\nenabled = true'))
            return result

        with mock.patch.object(self, "command", side_effect=command):
            with self.assertRaisesRegex(RuntimeError, "persisted opt-outs changed"):
                self.execute_offline()
        self.assertEqual(self.walk.env["CTX_ANALYTICS_ENABLED"], "false")

    def test_cli_accepts_2_0_2_and_keeps_recovery_explicit(self):
        for extra in ([], ["--installer-recovery"]):
            with self.subTest(extra=extra), mock.patch.object(WALK.Walk, "execute") as execute:
                output = self.root / ("recovery" if extra else "ordinary")
                WALK.main(["2.0.2", "2.2.0", "--output-dir", str(output), *extra])
                execute.assert_called_once()
                report = json.loads((output / "result.json").read_text())
                self.assertEqual(report["kind"], "released-installer-recovery" if extra
                                 else "manual-released-upgrade-walk")
        for args in (["2.0.2", "2.2.0", "--installer", "reviewed.sh"],
                     ["1.6.3", "2.2.0", "--installer-recovery"],
                     ["2.0.2", "2.2.0", "2.2.1", "--installer-recovery"]):
            with self.subTest(args=args), mock.patch.object(WALK, "Walk") as walk:
                with self.assertRaises(SystemExit):
                    WALK.main(args)
                walk.assert_not_called()

    def test_failed_import_still_stops_its_worker_and_preserves_error(self):
        original = self.command

        def command(label, args, env=None):
            if label == "import":
                self.walk.report["stage"] = label
                raise subprocess.TimeoutExpired(args, 1)
            return original(label, args, env)

        with mock.patch.object(self, "command", side_effect=command):
            with self.assertRaises(subprocess.TimeoutExpired):
                self.execute_offline()
        self.assertEqual(sum(args[1] == "daemon" for args, _ in self.commands), 1)
        self.assertEqual(self.walk.report["stage"], "import")

    def test_feed_skipping_bridge_is_not_repaired_or_reinstalled(self):
        self.updates = iter(["2.0.4"])
        with self.assertRaisesRegex(RuntimeError, "expected ctx 1.6.4, got ctx 2.0.4"):
            self.execute_offline()
        self.assertEqual(sum(args[0] == "sh" for args, _ in self.commands), 1)
        self.assertEqual(self.walk.report["status"], "failed")

    def test_updater_size_failure_surfaces_before_expected_version_check(self):
        self.updates = iter([RuntimeError("artifact exceeds 128 MiB")])
        with self.assertRaisesRegex(RuntimeError, "artifact exceeds 128 MiB"):
            self.execute_offline()
        self.assertEqual(self.walk.report["observed_versions"], ["ctx 1.6.3", "ctx 1.6.3"])
        self.assertEqual(sum(args[1] == "upgrade" for args, _ in self.commands), 1)
        self.assertEqual(self.walk.report["failed_hop_retention"]["status"], "passed")
        self.assertEqual(self.walk.report["stage"], "hop-1")

    def test_failed_retention_does_not_mask_original_updater_error(self):
        original = self.command

        def command(label, args, env=None):
            if label == "hop-1":
                self.walk.report["stage"] = label
                self.walk.binary.write_text("damaged")
                self.missing_history = True
                raise RuntimeError("original updater failure")
            return original(label, args, env)

        with mock.patch.object(self, "command", side_effect=command):
            with self.assertRaisesRegex(RuntimeError, "original updater failure"):
                self.execute_offline()
        retention = self.walk.report["failed_hop_retention"]
        self.assertEqual(retention["status"], "failed")
        self.assertIn("changed executable", retention["binary_error"])
        self.assertIn("no longer searchable", retention["history_error"])

    def test_reported_version_cannot_hide_wrong_artifact(self):
        self.tamper = True
        with self.assertRaisesRegex(RuntimeError, "checksum differs"):
            self.execute_offline()

    def test_missing_history_and_nonterminal_feed_fail(self):
        self.missing_history = True
        with self.assertRaisesRegex(RuntimeError, "no longer searchable"):
            self.execute_offline()
        self.missing_history = False
        self.terminal_status = "available"
        with self.assertRaisesRegex(RuntimeError, "did not report up_to_date"):
            self.execute_offline()

    def test_environment_discards_credentials_feed_overrides_and_provider_roots(self):
        with mock.patch.dict(os.environ, {"CTX_RELEASE_METADATA_URL": "https://fake.invalid",
                                         "CTX_ANALYTICS_ENABLED": "true", "BASH_ENV": "/secret",
                                         "CODEX_HOME": "/real-history", "TOKEN": "private"}):
            env = WALK.isolated_environment(self.root)
        for key in ("TOKEN", "BASH_ENV", "CTX_RELEASE_METADATA_URL", "HTTPS_PROXY"):
            self.assertNotIn(key, env)
        for key in ("HOME", "CTX_DATA_ROOT", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME",
                    "XDG_CACHE_HOME", "XDG_RUNTIME_DIR", "TMPDIR", "CODEX_HOME", "CLAUDE_CONFIG_DIR"):
            self.assertTrue(Path(env[key]).is_relative_to(self.root))
        self.assertEqual(env["CTX_ANALYTICS_ENABLED"], "false")
        self.assertEqual(env["CTX_UPGRADE_AUTO"], "off")

    def test_failed_subprocess_retains_diagnostic_and_timeout_is_bounded(self):
        with self.assertRaisesRegex(RuntimeError, "exited 7"):
            self.walk.run("failure", [sys.executable, "-c", "import sys; print('size limit', file=sys.stderr); sys.exit(7)"])
        self.assertIn("size limit", (self.root / "01-failure.stderr").read_text())
        with self.assertRaises(subprocess.TimeoutExpired):
            self.walk.run("timeout", [sys.executable, "-c", "import time; time.sleep(30)"])
        self.assertTrue(self.walk.report["commands"][-1]["timed_out"])

    def test_signed_metadata_parser_does_not_evaluate_shell_or_accept_duplicates(self):
        self.assertEqual(WALK.parse_metadata("# comment\nA=value\nB=$(false)\n"),
                         {"A": "value", "B": "$(false)"})
        for invalid in ("A=x\nA=y", "missing separator", " A=x", "A= x"):
            with self.assertRaisesRegex(RuntimeError, "invalid or duplicate"):
                WALK.parse_metadata(invalid)

    @unittest.skipUnless(sys.platform == "linux", "uses Linux process state to distinguish zombies")
    def test_exited_parent_does_not_leave_term_ignoring_descendant(self):
        program = """
import os, signal, time
reader, writer = os.pipe()
child = os.fork()
if child == 0:
    os.close(reader)
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    os.write(writer, b'ready')
    while True:
        time.sleep(1)
os.close(writer)
os.read(reader, 5)
print(child, flush=True)
"""
        child = int(self.walk.run("orphan", [sys.executable, "-c", program]).strip())
        def cleanup():
            try:
                os.kill(child, signal.SIGKILL)
            except ProcessLookupError:
                pass
        self.addCleanup(cleanup)
        # SIGKILL is asynchronous; allow only a bounded scheduling interval.
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            try:
                state = Path(f"/proc/{child}/stat").read_text().split(") ", 1)[1].split()[0]
            except FileNotFoundError:
                return
            if state == "Z":
                return
            time.sleep(0.01)
        self.fail("runner left a live descendant after the command parent exited")


if __name__ == "__main__":
    unittest.main()
