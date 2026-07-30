#!/usr/bin/env python3
"""Small real-binary query/show resource sanity for the nightly test tier."""

from __future__ import annotations

from dataclasses import dataclass
import datetime as dt
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest


EVENT_COUNT = 64
QUERY = "nightly performance sentinel"
COMMAND_TIMEOUT_SECONDS = 30.0
MAX_COMMAND_SECONDS = 15.0
MAX_PEAK_RSS_BYTES = 512 * 1024 * 1024
SAMPLE_COUNT = 3


def ctx_binary_argument() -> Path:
    if len(sys.argv) < 2 or sys.argv[1].startswith("-"):
        raise SystemExit("usage: performance_sanity_test.py PATH_TO_CTX")
    path = Path(sys.argv.pop(1)).resolve()
    if not path.is_file():
        raise SystemExit(f"ctx binary does not exist: {path}")
    return path


CTX_BIN = ctx_binary_argument()


@dataclass(frozen=True)
class CommandSample:
    packet: dict[str, object]
    elapsed_seconds: float
    peak_rss_bytes: int | None


def json_line(value: object) -> str:
    return json.dumps(value, separators=(",", ":"), sort_keys=True) + "\n"


def write_codex_fixture(home: Path) -> tuple[Path, int]:
    session_path = (
        home
        / ".codex"
        / "sessions"
        / "2026"
        / "07"
        / "30"
        / "nightly-performance.jsonl"
    )
    session_path.parent.mkdir(parents=True)
    session_id = "019fb4a0-1111-7777-8888-000000000001"
    base = dt.datetime(2026, 7, 30, 12, tzinfo=dt.timezone.utc)
    lines = [
        json_line(
            {
                "timestamp": base.isoformat().replace("+00:00", "Z"),
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "timestamp": base.isoformat().replace("+00:00", "Z"),
                    "cwd": "/workspace/ctx",
                    "originator": "codex-cli",
                    "cli_version": "1.0.0-test",
                    "source": "cli",
                    "model_provider": "openai",
                },
            }
        )
    ]
    for index in range(EVENT_COUNT):
        instant = base + dt.timedelta(milliseconds=index + 1)
        assistant = index % 2 == 1
        lines.append(
            json_line(
                {
                    "timestamp": instant.isoformat().replace("+00:00", "Z"),
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant" if assistant else "user",
                        "content": [
                            {
                                "type": "output_text" if assistant else "input_text",
                                "text": f"{QUERY} event {index:03d}",
                            }
                        ],
                        **({"phase": "commentary"} if assistant else {}),
                    },
                }
            )
        )
    body = "".join(lines).encode()
    session_path.write_bytes(body)
    return session_path, len(body)


def isolated_env(root: Path, home: Path) -> dict[str, str]:
    temp_root = root / "tmp"
    temp_root.mkdir()
    env = os.environ.copy()
    env.update(
        {
            "HOME": str(home),
            "CODEX_HOME": str(home / ".codex"),
            "CTX_ANALYTICS_ENABLED": "false",
            "CTX_DAEMON_MODE": "source-refresh-only",
            "CTX_DATA_ROOT": str(root / "data"),
            "CTX_UPGRADE_AUTO": "off",
            "NO_COLOR": "1",
            "TMPDIR": str(temp_root),
            "XDG_CACHE_HOME": str(home / ".cache"),
            "XDG_CONFIG_HOME": str(home / ".config"),
            "XDG_DATA_HOME": str(home / ".local" / "share"),
        }
    )
    env.pop("CODEX_THREAD_ID", None)
    return env


def command_failure(
    args: list[str], returncode: int, stdout: bytes, stderr: bytes
) -> RuntimeError:
    return RuntimeError(
        f"{' '.join(args)} exited {returncode}\n"
        f"stdout:\n{stdout.decode(errors='replace')}\n"
        f"stderr:\n{stderr.decode(errors='replace')}"
    )


def run_checked(args: list[str], env: dict[str, str], cwd: Path) -> bytes:
    completed = subprocess.run(
        [str(CTX_BIN), *args],
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=COMMAND_TIMEOUT_SECONDS,
        check=False,
    )
    if completed.returncode != 0:
        raise command_failure(
            args, completed.returncode, completed.stdout, completed.stderr
        )
    return completed.stdout


def linux_peak_rss_bytes(pid: int) -> int | None:
    status_path = Path("/proc") / str(pid) / "status"
    try:
        fields = status_path.read_text(encoding="ascii").splitlines()
    except (FileNotFoundError, PermissionError, ProcessLookupError):
        return None
    values: dict[str, int] = {}
    for line in fields:
        name, separator, raw = line.partition(":")
        if separator and name in {"VmHWM", "VmRSS"}:
            parts = raw.split()
            if len(parts) == 2 and parts[1] == "kB":
                values[name] = int(parts[0]) * 1024
    return values.get("VmHWM", values.get("VmRSS"))


def run_measured(
    args: list[str], env: dict[str, str], cwd: Path
) -> CommandSample:
    started = time.monotonic()
    with tempfile.TemporaryFile(mode="w+b", dir=cwd) as stdout_file, (
        tempfile.TemporaryFile(mode="w+b", dir=cwd)
    ) as stderr_file:
        process = subprocess.Popen(
            [str(CTX_BIN), *args],
            cwd=cwd,
            env=env,
            stdout=stdout_file,
            stderr=stderr_file,
        )
        peak_rss_bytes: int | None = None
        deadline = started + COMMAND_TIMEOUT_SECONDS
        while process.poll() is None:
            observed = linux_peak_rss_bytes(process.pid)
            if observed is not None:
                peak_rss_bytes = max(peak_rss_bytes or 0, observed)
            if time.monotonic() >= deadline:
                process.kill()
                process.wait()
                stdout_file.seek(0)
                stderr_file.seek(0)
                raise TimeoutError(
                    f"{' '.join(args)} exceeded {COMMAND_TIMEOUT_SECONDS}s\n"
                    f"stdout:\n{stdout_file.read().decode(errors='replace')}\n"
                    f"stderr:\n{stderr_file.read().decode(errors='replace')}"
                )
            time.sleep(0.002)
        stdout_file.seek(0)
        stderr_file.seek(0)
        stdout = stdout_file.read()
        stderr = stderr_file.read()
    elapsed_seconds = time.monotonic() - started
    if process.returncode != 0:
        raise command_failure(args, process.returncode, stdout, stderr)
    packet = json.loads(stdout)
    if not isinstance(packet, dict):
        raise RuntimeError(f"{' '.join(args)} did not return a JSON object")
    return CommandSample(packet, elapsed_seconds, peak_rss_bytes)


def start_daemon(
    root: Path, env: dict[str, str]
) -> tuple[subprocess.Popen[bytes], object, object]:
    stdout_file = (root / "daemon.stdout").open("w+b")
    stderr_file = (root / "daemon.stderr").open("w+b")
    process = subprocess.Popen(
        [
            str(CTX_BIN),
            "daemon",
            "run",
            "--force",
            "--idle-exit-seconds",
            "60",
            "--loop-interval-seconds",
            "300",
            "--format=json",
        ],
        cwd=root,
        env=env,
        stdout=stdout_file,
        stderr=stderr_file,
    )
    deadline = time.monotonic() + COMMAND_TIMEOUT_SECONDS
    last_status: object = None
    while time.monotonic() < deadline:
        if process.poll() is not None:
            stdout_file.seek(0)
            stderr_file.seek(0)
            raise command_failure(
                ["daemon", "run"],
                process.returncode,
                stdout_file.read(),
                stderr_file.read(),
            )
        try:
            status = json.loads(
                run_checked(["daemon", "status", "--format=json"], env, root)
            )
            last_status = status
        except (RuntimeError, subprocess.TimeoutExpired, json.JSONDecodeError):
            time.sleep(0.02)
            continue
        daemon = status.get("daemon", {})
        endpoint = (
            daemon.get("source_refresh_endpoint", {})
            if isinstance(daemon, dict)
            else {}
        )
        if (
            daemon.get("running") is True
            and endpoint.get("available") is True
        ):
            return process, stdout_file, stderr_file
        time.sleep(0.02)
    process.terminate()
    process.wait(timeout=5)
    stdout_file.seek(0)
    stderr_file.seek(0)
    raise TimeoutError(
        "source-refresh daemon did not become ready\n"
        f"last status:\n{json.dumps(last_status, indent=2, sort_keys=True)}\n"
        f"stdout:\n{stdout_file.read().decode(errors='replace')}\n"
        f"stderr:\n{stderr_file.read().decode(errors='replace')}"
    )


def stop_daemon(
    process: subprocess.Popen[bytes], stdout_file: object, stderr_file: object
) -> None:
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
    stdout_file.close()
    stderr_file.close()


class SmallQueryShowPerformanceTest(unittest.TestCase):
    def test_small_query_and_show_stay_within_sanity_bounds(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ctx-performance-sanity-") as temporary:
            root = Path(temporary)
            home = root / "home"
            home.mkdir()
            _fixture_path, fixture_bytes = write_codex_fixture(home)
            env = isolated_env(root, home)
            run_checked(
                ["setup", "--catalog-only", "--no-daemon", "--progress", "none"],
                env,
                root,
            )
            daemon, daemon_stdout, daemon_stderr = start_daemon(root, env)
            try:
                initial = json.loads(
                    run_checked(
                        [
                            "search",
                            QUERY,
                            "--refresh",
                            "wait",
                            "--format=json",
                            "--limit",
                            "1",
                        ],
                        env,
                        root,
                    )
                )
                self.assertTrue(initial.get("results"))
                search_samples = [
                    run_measured(
                        [
                            "search",
                            QUERY,
                            "--refresh",
                            "off",
                            "--format=json",
                            "--limit",
                            "10",
                        ],
                        env,
                        root,
                    )
                    for _ in range(SAMPLE_COUNT)
                ]
                results = search_samples[-1].packet.get("results")
                self.assertIsInstance(results, list)
                self.assertTrue(results)
                session_id = results[0].get("ctx_session_id")
                self.assertIsInstance(session_id, str)
                self.assertTrue(session_id)
                show_samples = [
                    run_measured(
                        [
                            "show",
                            "session",
                            session_id,
                            "--mode",
                            "lite",
                            "--format",
                            "json",
                        ],
                        env,
                        root,
                    )
                    for _ in range(SAMPLE_COUNT)
                ]
            finally:
                stop_daemon(daemon, daemon_stdout, daemon_stderr)

        shown_id = show_samples[-1].packet.get(
            "ctx_session_id", show_samples[-1].packet.get("id")
        )
        self.assertEqual(shown_id, session_id)
        self.assertLessEqual(
            max(sample.elapsed_seconds for sample in search_samples),
            MAX_COMMAND_SECONDS,
        )
        self.assertLessEqual(
            max(sample.elapsed_seconds for sample in show_samples),
            MAX_COMMAND_SECONDS,
        )
        measured_rss = [
            sample.peak_rss_bytes
            for sample in (*search_samples, *show_samples)
            if sample.peak_rss_bytes is not None
        ]
        if sys.platform.startswith("linux"):
            self.assertEqual(len(measured_rss), SAMPLE_COUNT * 2)
        for peak_rss_bytes in measured_rss:
            self.assertLessEqual(peak_rss_bytes, MAX_PEAK_RSS_BYTES)

        search_max = max(sample.elapsed_seconds for sample in search_samples)
        show_max = max(sample.elapsed_seconds for sample in show_samples)
        rss_max = max(measured_rss, default=0)
        print(
            "performance sanity:"
            f" fixture_events={EVENT_COUNT}"
            f" fixture_bytes={fixture_bytes}"
            f" search_max_seconds={search_max:.3f}"
            f" show_max_seconds={show_max:.3f}"
            f" peak_rss_bytes={rss_max}"
        )


if __name__ == "__main__":
    unittest.main()
