from __future__ import annotations

import ctypes
import json
import os
from pathlib import Path
import sys
import tempfile
import textwrap
import time
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

import ctx_agent_history._subprocess as subprocess_impl
from ctx_agent_history.config import LocalConfig
from ctx_agent_history.errors import (
    CtxAgentHistoryCliError,
    CtxAgentHistoryProtocolError,
    CtxAgentHistoryTimeoutError,
)
from ctx_agent_history.transport import LocalCliAdapter


class LocalCliSubprocessTests(unittest.TestCase):
    def test_stdout_limit_matches_cli_presentation_contract(self) -> None:
        self.assertEqual(subprocess_impl._STDOUT_CAP_BYTES, 64 * 1024 * 1024)

    def test_success_preserves_exact_large_json_while_draining_both_pipes(self) -> None:
        payload_bytes = 3 * 1024 * 1024
        stderr_bytes = 200_000
        adapter = self._python_adapter(timeout=5)
        source = textwrap.dedent(
            f"""\
            import json
            import os
            import threading

            stdout = json.dumps(
                {{"payload": "x" * {payload_bytes}}},
                separators=(",", ":"),
            ).encode()
            stderr = b"e" * {stderr_bytes}

            def write_all(descriptor, data):
                view = memoryview(data)
                while view:
                    view = view[os.write(descriptor, view):]

            threads = [
                threading.Thread(target=write_all, args=(1, stdout)),
                threading.Thread(target=write_all, args=(2, stderr)),
            ]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join()
            """
        )

        completed = adapter._run(["-c", source])

        self.assertEqual(json.loads(completed.stdout), {"payload": "x" * payload_bytes})
        self.assertEqual(completed.stderr, "e" * stderr_bytes)

    def test_oversized_streams_drain_to_completion_with_bounded_details(self) -> None:
        adapter = self._python_adapter(timeout=5)
        for stream, expression, cap_name, cap in (
            ("stdout", "sys.stdout.buffer", "_STDOUT_CAP_BYTES", 128 * 1024),
            ("stderr", "sys.stderr.buffer", "_STDERR_CAP_BYTES", 128 * 1024),
        ):
            with (
                self.subTest(stream=stream),
                tempfile.TemporaryDirectory() as directory,
                mock.patch.object(subprocess_impl, cap_name, cap),
            ):
                pid_path = Path(directory) / "process.pid"
                completed_path = Path(directory) / "process.completed"
                source = textwrap.dedent(
                    f"""\
                    import os
                    from pathlib import Path
                    import sys
                    import time

                    Path(sys.argv[1]).write_text(str(os.getpid()), encoding="utf-8")
                    output = {expression}
                    output.write(b"x" * {cap + 512 * 1024})
                    output.flush()
                    time.sleep(0.05)
                    Path(sys.argv[2]).write_text("completed", encoding="utf-8")
                    """
                )
                started = time.monotonic()

                with self.assertRaises(CtxAgentHistoryProtocolError) as raised:
                    adapter._run(["-c", source, str(pid_path), str(completed_path)])

                elapsed = time.monotonic() - started
                self.assertEqual(
                    raised.exception.message,
                    "ctx CLI output exceeded its capture limit",
                )
                self.assertEqual(raised.exception.details["stream"], stream)
                self.assertEqual(raised.exception.details["cap_bytes"], cap)
                self.assertNotIn("stdout", raised.exception.details)
                self.assertNotIn("stderr", raised.exception.details)
                self.assertLess(elapsed, 2)
                self.assertEqual(completed_path.read_text(encoding="utf-8"), "completed")
                self._assert_process_stopped(int(pid_path.read_text(encoding="utf-8")))

    @unittest.skipUnless(os.name == "posix", "POSIX process-group behavior")
    def test_timeout_covers_pipe_eof_and_kills_persistent_descendant(self) -> None:
        adapter = self._python_adapter(timeout=0.6)
        with tempfile.TemporaryDirectory() as directory:
            pid_path = Path(directory) / "descendant.pid"
            ready_path = Path(directory) / "descendant.ready"
            source = textwrap.dedent(
                """\
                import os
                from pathlib import Path
                import signal
                import subprocess
                import sys
                import time

                child_source = '''
                from pathlib import Path
                import signal
                import sys
                import time
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
                Path(sys.argv[1]).write_text("ready", encoding="utf-8")
                time.sleep(60)
                '''
                child = subprocess.Popen(
                    [sys.executable, "-c", child_source, sys.argv[2]],
                    stdin=subprocess.DEVNULL,
                    stdout=sys.stdout,
                    stderr=sys.stderr,
                )
                while not os.path.exists(sys.argv[2]):
                    time.sleep(0.005)
                Path(sys.argv[1]).write_text(str(child.pid), encoding="utf-8")
                print("{}", flush=True)
                """
            )
            started = time.monotonic()

            with self.assertRaises(CtxAgentHistoryTimeoutError) as raised:
                adapter._run(["-c", source, str(pid_path), str(ready_path)])

            elapsed = time.monotonic() - started
            self.assertEqual(raised.exception.message, "ctx CLI timed out")
            self.assertEqual(raised.exception.details["stdout"], "{}\n")
            self.assertEqual(raised.exception.details["stderr"], "")
            self.assertEqual(raised.exception.details["timeout"], 0.6)
            self.assertGreaterEqual(elapsed, 0.5)
            self.assertLess(elapsed, 2)
            self._assert_process_stopped(int(pid_path.read_text(encoding="utf-8")))

    @unittest.skipUnless(os.name == "posix", "POSIX signal escalation behavior")
    def test_timeout_escalates_after_process_ignores_term_and_reaps_it(self) -> None:
        adapter = self._python_adapter(timeout=0.4)
        with tempfile.TemporaryDirectory() as directory:
            pid_path = Path(directory) / "process.pid"
            term_path = Path(directory) / "term.received"
            source = textwrap.dedent(
                """\
                import os
                from pathlib import Path
                import signal
                import sys
                import time

                def ignore_term(signum, frame):
                    del signum, frame
                    Path(sys.argv[2]).write_text("term", encoding="utf-8")

                signal.signal(signal.SIGTERM, ignore_term)
                Path(sys.argv[1]).write_text(str(os.getpid()), encoding="utf-8")
                print("partial", flush=True)
                time.sleep(60)
                """
            )

            with self.assertRaises(CtxAgentHistoryTimeoutError) as raised:
                adapter._run(["-c", source, str(pid_path), str(term_path)])

            self.assertEqual(raised.exception.details["stdout"], "partial\n")
            self.assertEqual(term_path.read_text(encoding="utf-8"), "term")
            self._assert_process_stopped(int(pid_path.read_text(encoding="utf-8")))

    @unittest.skipUnless(os.name == "posix", "POSIX process-group behavior")
    def test_success_does_not_orphan_descendant_with_closed_pipes(self) -> None:
        adapter = self._python_adapter(timeout=2)
        with tempfile.TemporaryDirectory() as directory:
            pid_path = Path(directory) / "descendant.pid"
            source = textwrap.dedent(
                """\
                from pathlib import Path
                import signal
                import subprocess
                import sys

                child = subprocess.Popen(
                    [
                        sys.executable,
                        "-c",
                        "import signal,time; "
                        "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
                        "time.sleep(60)",
                    ],
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
                Path(sys.argv[1]).write_text(str(child.pid), encoding="utf-8")
                print("{}")
                """
            )

            completed = adapter._run(["-c", source, str(pid_path)])

            self.assertEqual(completed.stdout, "{}\n")
            self._assert_process_stopped(int(pid_path.read_text(encoding="utf-8")))

    def test_nonzero_exit_preserves_existing_cli_error_contract(self) -> None:
        adapter = self._python_adapter(timeout=2)
        source = (
            "import sys; sys.stdout.write('partial out'); "
            "sys.stderr.write('boom\\n'); sys.exit(42)"
        )

        with self.assertRaises(CtxAgentHistoryCliError) as raised:
            adapter._run(["-c", source])

        self.assertEqual(raised.exception.message, "ctx CLI command failed")
        self.assertEqual(raised.exception.code, "adapter_error")
        self.assertEqual(raised.exception.exit_code, 42)
        self.assertEqual(raised.exception.stdout, "partial out")
        self.assertEqual(raised.exception.stderr, "boom\n")
        self.assertEqual(raised.exception.details["stdout"], "partial out")
        self.assertEqual(raised.exception.details["stderr"], "boom\n")

    def test_malformed_json_preserves_existing_protocol_error_contract(self) -> None:
        adapter = self._python_adapter(timeout=2)
        args = ["-c", "print('not json')"]

        with self.assertRaises(CtxAgentHistoryProtocolError) as raised:
            adapter._json(args)

        self.assertEqual(raised.exception.message, "ctx returned invalid JSON")
        self.assertEqual(raised.exception.code, "decode_error")
        self.assertEqual(raised.exception.details["command"], adapter._command(args))
        self.assertEqual(raised.exception.details["stdout"], "not json\n")
        self.assertEqual(raised.exception.details["stderr"], "")

    def _python_adapter(self, *, timeout: float) -> LocalCliAdapter:
        return LocalCliAdapter(LocalConfig(ctx_binary=sys.executable, timeout=timeout))

    def _assert_process_stopped(self, pid: int) -> None:
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            if not self._process_is_running(pid):
                return
            time.sleep(0.01)
        self.fail(f"owned process {pid} survived bounded teardown")

    def _process_is_running(self, pid: int) -> bool:
        if os.name != "nt":
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return False
            stat_path = Path(f"/proc/{pid}/stat")
            if stat_path.exists():
                try:
                    return stat_path.read_text(encoding="utf-8").split()[2] != "Z"
                except (OSError, IndexError):
                    pass
            return True

        synchronize = 0x00100000
        wait_timeout = 0x00000102
        kernel32 = ctypes.windll.kernel32  # type: ignore[attr-defined]
        handle = kernel32.OpenProcess(synchronize, False, pid)
        if not handle:
            return False
        try:
            return kernel32.WaitForSingleObject(handle, 0) == wait_timeout
        finally:
            kernel32.CloseHandle(handle)


if __name__ == "__main__":
    unittest.main()
