#!/usr/bin/env python3
from __future__ import annotations

import base64
import errno
import hashlib
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path


sys.dont_write_bytecode = True
TOOL_ROOT = Path(__file__).resolve().parent
FIXTURE = TOOL_ROOT / "fixtures" / "observer_fixture.py"
sys.path.insert(0, str(TOOL_ROOT))

import observe  # noqa: E402


@unittest.skipUnless(
    observe.isolation_supported(),
    "Linux PTY/seccomp observation is unavailable",
)
class ObserverTests(unittest.TestCase):
    def setUp(self) -> None:
        # Bazel's TEST_TMPDIR can exceed Linux's AF_UNIX pathname limit once
        # the observer adds its isolated capture-root and socket suffixes.
        self.temporary = tempfile.TemporaryDirectory(
            prefix="ctx-cli-ux-observe-test.", dir="/tmp"
        )
        self.addCleanup(self.temporary.cleanup)
        self.root_parent = Path(self.temporary.name)

    def run_fixture(
        self,
        mode: str,
        *arguments: str,
        columns: int = 91,
        rows: int = 27,
        timeout_ms: int = 3_000,
        profile: str = observe.PROFILE_NO_SOCKET,
        environment: dict[str, str] | None = None,
        stdin: bytes | str = b"",
    ) -> dict[str, object]:
        receipt = observe.observe(
            [sys.executable, str(FIXTURE), mode, *arguments],
            columns=columns,
            rows=rows,
            timeout_ms=timeout_ms,
            profile=profile,
            environment=environment,
            stdin=stdin,
            root_parent=self.root_parent,
        )
        self.assertEqual(
            list(self.root_parent.glob("ctx-cli-ux-observe.*")), []
        )
        return receipt

    def test_success_geometry_controls_ansi_and_hashes(self) -> None:
        receipt = self.run_fixture("controls")
        self.assertEqual(receipt["kind"], "ctx-cli-ux-observation")
        self.assertEqual(receipt["terminal"], {"columns": 91, "rows": 27})
        observed = receipt["observed"]
        self.assertEqual(
            observed["termination"], {"kind": "exit", "exit_code": 0}
        )
        self.assertEqual(observed["exit_code"], 0)
        stream = observed["raw_stream"]["text"]
        self.assertEqual(
            observed["frames"],
            [{"boundary": "eof", "delay_ms": 0, "data": stream}],
        )
        self.assertIn(
            "geometry:91x27 tty:[True, True, True]\r\n", stream
        )
        self.assertIn("\x1b[31mred 日本語✓\x1b[0m\r\n", stream)
        self.assertIn("\x1b]0;fixture title\x07", stream)
        self.assertIn("\x1b[?25lcontrol\x1b[?25h\r\n", stream)
        self.assertTrue(stream.endswith("stderr\r\n"))
        raw = stream.encode("utf-8")
        self.assertEqual(
            observed["raw_stream"]["base64"],
            base64.b64encode(raw).decode("ascii"),
        )
        self.assertEqual(observed["raw_stream"]["utf8_bytes"], len(raw))
        self.assertEqual(
            observed["raw_stream"]["sha256"],
            hashlib.sha256(raw).hexdigest(),
        )
        self.assertEqual(
            observed["plain_projection"]["text"],
            (
                "geometry:91x27 tty:[True, True, True]\n"
                "red 日本語✓\n"
                "control\n"
                "stderr\n"
            ),
        )
        plain = observed["plain_projection"]["text"].encode()
        self.assertEqual(
            observed["plain_projection"]["utf8_bytes"], len(plain)
        )
        self.assertEqual(
            observed["plain_projection"]["sha256"],
            hashlib.sha256(plain).hexdigest(),
        )
        self.assertEqual(
            receipt["cleanup"],
            {
                "descendant_processes_detected": 0,
                "descendant_processes_remaining": 0,
                "root_removed": True,
            },
        )

    def test_exact_argv_environment_allowlist_and_stdin_digest(self) -> None:
        secret = os.environ.get("CLI_UX_AMBIENT_SECRET")
        os.environ["CLI_UX_AMBIENT_SECRET"] = "must-not-leak"
        try:
            receipt = self.run_fixture(
                "environment",
                environment={
                    "CTX_FIXTURE_VALUE": "${CAPTURE_ROOT}/fixture"
                },
            )
        finally:
            if secret is None:
                os.environ.pop("CLI_UX_AMBIENT_SECRET", None)
            else:
                os.environ["CLI_UX_AMBIENT_SECRET"] = secret
        inputs = receipt["inputs"]
        self.assertEqual(inputs["requested_argv"][1:], [str(FIXTURE), "environment"])
        self.assertEqual(inputs["executed_argv"][1:], [str(FIXTURE), "environment"])
        self.assertEqual(
            inputs["argv_sha256"],
            observe._canonical_json_sha256(inputs["executed_argv"]),
        )
        allowlist = inputs["environment"]["allowlist"]
        self.assertEqual(allowlist["HOME"], "${CAPTURE_ROOT}/home")
        self.assertEqual(
            allowlist["CTX_FIXTURE_VALUE"], "${CAPTURE_ROOT}/fixture"
        )
        self.assertEqual(allowlist["CTX_CLI_UX_CAPTURE_ID"], "${CAPTURE_ID}")
        self.assertNotIn("CLI_UX_AMBIENT_SECRET", allowlist)
        self.assertEqual(
            inputs["environment"]["sha256"],
            observe._canonical_json_sha256(allowlist),
        )
        lines = receipt["observed"]["plain_projection"]["text"].splitlines()
        child_environment = json.loads(lines[0])
        self.assertEqual(lines[1], "ambient-secret:absent")
        for name in ("HOME", "XDG_CONFIG_HOME", "CTX_DATA_ROOT", "TMPDIR"):
            self.assertIn("ctx-cli-ux-observe.", child_environment[name])
        self.assertTrue(
            child_environment["CTX_FIXTURE_VALUE"].endswith("/fixture")
        )

        stdin_receipt = self.run_fixture("stdin", stdin="hello\n")
        stdin_bytes = b"hello\n"
        self.assertEqual(
            stdin_receipt["inputs"]["stdin"],
            {
                "bytes": len(stdin_bytes),
                "sha256": hashlib.sha256(stdin_bytes).hexdigest(),
            },
        )
        self.assertEqual(
            stdin_receipt["observed"]["plain_projection"]["text"],
            "stdin:'hello\\n'\n",
        )

    def test_failure_signal_and_timeout_are_distinct(self) -> None:
        failed = self.run_fixture("exit")
        self.assertEqual(failed["observed"]["exit_code"], 7)
        self.assertEqual(
            failed["observed"]["termination"],
            {"kind": "exit", "exit_code": 7},
        )
        self.assertEqual(
            failed["observed"]["plain_projection"]["text"], "failure\n"
        )

        signaled = self.run_fixture("signal")
        self.assertIsNone(signaled["observed"]["exit_code"])
        self.assertEqual(
            signaled["observed"]["termination"],
            {
                "kind": "signal",
                "signal": signal.SIGTERM,
                "signal_name": "SIGTERM",
            },
        )

        timed_out = self.run_fixture("timeout", timeout_ms=80)
        self.assertEqual(
            timed_out["observed"]["termination"]["kind"], "timeout"
        )
        self.assertEqual(
            timed_out["observed"]["termination"]["timeout_ms"], 80
        )
        self.assertEqual(
            timed_out["observed"]["termination"]["signal"], signal.SIGKILL
        )
        self.assertEqual(
            timed_out["observed"]["plain_projection"]["text"], "waiting\n"
        )

    def test_no_socket_profile_denies_unix_and_socketpair(self) -> None:
        receipt = self.run_fixture("socket-denied")
        self.assertEqual(
            receipt["inputs"]["socket_policy"], "deny-all"
        )
        self.assertEqual(
            receipt["observed"]["plain_projection"]["text"],
            f"unix:{errno.EPERM}\nsocketpair:{errno.EPERM}\n",
        )

    def test_local_unix_profile_allows_local_daemon_and_denies_network(self) -> None:
        receipt = self.run_fixture(
            "local-unix", profile=observe.PROFILE_LOCAL_UNIX
        )
        self.assertEqual(
            receipt["inputs"]["socket_policy"], "allow-af-unix-only"
        )
        lines = receipt["observed"]["plain_projection"]["text"].splitlines()
        self.assertEqual(lines[0], "unix:local")
        self.assertEqual(
            lines[1:],
            [
                f"inet-stream:{errno.EPERM}",
                f"inet-dgram:{errno.EPERM}",
                f"inet6-stream:{errno.EPERM}",
                f"netlink:{errno.EPERM}",
            ],
        )

    def test_invalid_utf8_and_ambient_roots_are_rejected(self) -> None:
        with self.assertRaisesRegex(
            observe.ObservationError, "not valid UTF-8"
        ):
            self.run_fixture("invalid-utf8")
        self.assertEqual(
            list(self.root_parent.glob("ctx-cli-ux-observe.*")), []
        )
        for control in ("value\x00", "value\x1b", "value\x1b[31"):
            with self.subTest(control=repr(control)):
                with self.assertRaises(observe.ObservationError):
                    observe.plain_projection(control)
        for value in ("~/state", "/tmp/.ctx/state", str(Path.home() / "state")):
            with self.subTest(value=value):
                with self.assertRaisesRegex(
                    observe.ObservationError, "ambient"
                ):
                    self.run_fixture(
                        "environment",
                        environment={"CTX_FIXTURE_VALUE": value},
                    )
        with self.assertRaisesRegex(
            observe.ObservationError, "not explicitly allowlisted"
        ):
            self.run_fixture(
                "environment", environment={"HOME": "/tmp/not-allowed"}
            )

    def test_orphan_is_killed_reaped_and_rejected_without_root_leak(self) -> None:
        pid_file = self.root_parent / "orphan.pid"
        with self.assertRaisesRegex(
            observe.ObservationError, "leaked descendant"
        ):
            self.run_fixture("orphan", str(pid_file))
        child_pid = int(pid_file.read_text(encoding="ascii"))
        deadline = time.monotonic() + 1
        while Path(f"/proc/{child_pid}").exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertFalse(Path(f"/proc/{child_pid}").exists())
        self.assertEqual(
            list(self.root_parent.glob("ctx-cli-ux-observe.*")), []
        )

    def test_exited_adopted_child_is_reaped_before_leak_classification(self) -> None:
        receipt = self.run_fixture("exited-orphan")

        self.assertEqual(
            receipt["observed"]["termination"],
            {"kind": "exit", "exit_code": 0},
        )
        self.assertEqual(
            receipt["cleanup"]["descendant_processes_detected"], 0
        )
        self.assertEqual(
            receipt["cleanup"]["descendant_processes_remaining"], 0
        )

    def test_cli_writes_low_level_receipt_and_rejects_timeout(self) -> None:
        output = self.root_parent / "receipt.json"
        completed = subprocess.run(
            [
                sys.executable,
                str(TOOL_ROOT / "observe.py"),
                "--columns",
                "80",
                "--rows",
                "24",
                "--env",
                "CTX_FIXTURE_VALUE=explicit",
                "--output",
                str(output),
                "--",
                sys.executable,
                str(FIXTURE),
                "environment",
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=10,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        receipt = json.loads(output.read_text(encoding="utf-8"))
        self.assertEqual(receipt["kind"], "ctx-cli-ux-observation")
        self.assertNotIn("scenario", receipt)
        self.assertEqual(
            receipt["inputs"]["environment"]["allowlist"][
                "CTX_FIXTURE_VALUE"
            ],
            "explicit",
        )

        timeout_output = self.root_parent / "timeout.json"
        timed_out = subprocess.run(
            [
                sys.executable,
                str(TOOL_ROOT / "observe.py"),
                "--columns",
                "80",
                "--rows",
                "24",
                "--timeout-ms",
                "80",
                "--output",
                str(timeout_output),
                "--",
                sys.executable,
                str(FIXTURE),
                "timeout",
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=10,
        )
        self.assertEqual(timed_out.returncode, 124, timed_out.stderr)
        timeout_receipt = json.loads(
            timeout_output.read_text(encoding="utf-8")
        )
        self.assertEqual(
            timeout_receipt["observed"]["termination"]["kind"], "timeout"
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
