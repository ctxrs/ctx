"""Shared process and publication helpers for performance sanity tests."""

from __future__ import annotations

from dataclasses import dataclass
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time


COMMAND_TIMEOUT_SECONDS = 30.0
MAX_COMMAND_SECONDS = 15.0
MAX_PEAK_RSS_BYTES = 512 * 1024 * 1024
FORCE_SINGLE_CPU_ENV = "CTX_PERFORMANCE_FORCE_SINGLE_CPU"
TASK_BINARY_ENV = "CTX_PERFORMANCE_TASK_BINARY"


def ctx_binary_argument() -> Path:
    if len(sys.argv) < 2 or sys.argv[1].startswith("-"):
        raise SystemExit("usage: performance_sanity_test.py PATH_TO_CTX")
    path = Path(sys.argv.pop(1)).resolve()
    if not path.is_file():
        raise SystemExit(f"ctx binary does not exist: {path}")
    return path


CTX_BIN = ctx_binary_argument()


@dataclass(frozen=True)
class PublishedFileState:
    body: bytes
    modified_ns: int
    inode: int


@dataclass(frozen=True)
class RefreshSnapshot:
    request_id: str
    previous_generation: str | None
    generation_id: str
    generation_changed: bool
    indexed_documents: int
    current: dict[str, object]
    opstamp: int
    segments: tuple[str, ...]
    meta: PublishedFileState
    manifest: PublishedFileState
    manifest_names: tuple[str, ...]
    index_bytes: int


def isolated_env(root: Path, home: Path) -> dict[str, str]:
    temp_root = root / "tmp"
    temp_root.mkdir()
    task_binary = root / "ctx-test-binary"
    shutil.copyfile(CTX_BIN, task_binary)
    task_binary.chmod(0o700)
    env = os.environ.copy()
    env.update(
        {
            "HOME": str(home),
            "CODEX_HOME": str(home / ".codex"),
            "CTX_ANALYTICS_ENABLED": "false",
            "CTX_DAEMON_MODE": "source-refresh-only",
            "CTX_DATA_ROOT": str(root / "data"),
            TASK_BINARY_ENV: str(task_binary),
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


def task_binary(env: dict[str, str]) -> str:
    return env[TASK_BINARY_ENV]


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
        [task_binary(env), *args],
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


def run_json(args: list[str], env: dict[str, str], cwd: Path) -> dict[str, object]:
    packet = json.loads(run_checked(args, env, cwd))
    if not isinstance(packet, dict):
        raise RuntimeError(f"{' '.join(args)} did not return a JSON object")
    return packet


def run_json_timed(
    args: list[str], env: dict[str, str], cwd: Path
) -> tuple[dict[str, object], float]:
    started = time.monotonic()
    packet = run_json(args, env, cwd)
    return packet, time.monotonic() - started


def published_file_state(path: Path) -> PublishedFileState:
    metadata = path.stat()
    return PublishedFileState(
        body=path.read_bytes(),
        modified_ns=metadata.st_mtime_ns,
        inode=metadata.st_ino,
    )


def directory_bytes(path: Path) -> int:
    return sum(entry.stat().st_size for entry in path.rglob("*") if entry.is_file())


def refresh_snapshot(
    search: dict[str, object], root: Path, env: dict[str, str]
) -> RefreshSnapshot:
    retrieval = search["retrieval"]
    generation_id = retrieval["generation_id"]
    indexed_documents = retrieval["indexed_documents"]
    deadline = time.monotonic() + COMMAND_TIMEOUT_SECONDS
    while True:
        status = run_json(["status", "--format=json"], env, root)
        daemon = status["daemon"]
        job = daemon["jobs"]["source_backed_refresh"]
        if (
            daemon["mode"] == "source-refresh-only"
            and job["owner"] == "daemon"
            and job["status"] == "completed"
            and job["request_state"] == "published"
            and job["published_generation"] == generation_id
        ):
            break
        if job["status"] in {"failed", "retry_backoff"}:
            raise RuntimeError(
                f"refresh failed before publishing the queried generation: {job!r}"
            )
        if time.monotonic() >= deadline:
            raise RuntimeError(
                "refresh did not settle on the queried generation through the "
                f"ready daemon product seam: {job!r}"
            )
        time.sleep(0.025)
    receipt = job.get("receipt")
    if not isinstance(receipt, dict):
        raise RuntimeError(f"completed refresh omitted its receipt: {job!r}")
    current = receipt["current"]
    if (
        job["published_generation"] != generation_id
        or receipt["published_generation"] != generation_id
        or status["lexical"]["generation_id"] != generation_id
        or current["current_indexed_documents"] != indexed_documents
    ):
        raise RuntimeError(
            "search, status, receipt, and lexical generation facts disagree: "
            f"search={search!r}, status={status!r}"
        )

    index_root = Path(env["CTX_DATA_ROOT"]) / "search" / "lexical"
    meta = published_file_state(index_root / "meta.json")
    meta_packet = json.loads(meta.body)
    segments = tuple(
        sorted(segment["segment_id"] for segment in meta_packet["segments"])
    )
    manifest_directory = index_root / "ctx-generations"
    manifest_path = manifest_directory / f"{generation_id}.json"
    manifest = published_file_state(manifest_path)
    manifest_names = tuple(
        sorted(path.name for path in manifest_directory.iterdir() if path.is_file())
    )
    return RefreshSnapshot(
        request_id=job["request_id"],
        previous_generation=job.get("previous_generation"),
        generation_id=generation_id,
        generation_changed=job["generation_changed"],
        indexed_documents=indexed_documents,
        current=dict(current),
        opstamp=meta_packet["opstamp"],
        segments=segments,
        meta=meta,
        manifest=manifest,
        manifest_names=manifest_names,
        index_bytes=directory_bytes(index_root),
    )


def start_daemon(
    root: Path,
    env: dict[str, str],
    affinity: set[int] | None = None,
) -> tuple[subprocess.Popen[bytes], object, object]:
    stdout_file = (root / "daemon.stdout").open("w+b")
    stderr_file = (root / "daemon.stderr").open("w+b")
    process = subprocess.Popen(
        [
            task_binary(env),
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
        start_new_session=os.name == "posix",
    )
    if affinity is not None:
        os.sched_setaffinity(process.pid, affinity)
    deadline = time.monotonic() + COMMAND_TIMEOUT_SECONDS
    last_status: object = None
    while time.monotonic() < deadline:
        if process.poll() is not None:
            stdout_file.seek(0)
            stderr_file.seek(0)
            error = command_failure(
                ["daemon", "run"],
                process.returncode,
                stdout_file.read(),
                stderr_file.read(),
            )
            stdout_file.close()
            stderr_file.close()
            raise error
        try:
            status = run_json(["daemon", "status", "--format=json"], env, root)
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
        if daemon.get("running") is True and endpoint.get("available") is True:
            return process, stdout_file, stderr_file
        time.sleep(0.02)
    process.terminate()
    process.wait(timeout=5)
    stdout_file.seek(0)
    stderr_file.seek(0)
    error = TimeoutError(
        "source-refresh daemon did not become ready\n"
        f"last status:\n{json.dumps(last_status, indent=2, sort_keys=True)}\n"
        f"stdout:\n{stdout_file.read().decode(errors='replace')}\n"
        f"stderr:\n{stderr_file.read().decode(errors='replace')}"
    )
    stdout_file.close()
    stderr_file.close()
    raise error


def stop_daemon(
    process: subprocess.Popen[bytes],
    stdout_file: object,
    stderr_file: object,
    root: Path,
    env: dict[str, str],
) -> None:
    daemon_pid = process.pid
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
    stdout_file.close()
    stderr_file.close()
    status = run_json(["daemon", "status", "--format=json"], env, root)
    daemon = status.get("daemon", {})
    if isinstance(daemon, dict) and daemon.get("running") is True:
        raise RuntimeError(f"daemon {daemon_pid} remained live after teardown")
    if os.name == "posix":
        try:
            os.killpg(daemon_pid, 0)
        except ProcessLookupError:
            pass
        else:
            raise RuntimeError(
                f"daemon process group {daemon_pid} survived teardown"
            )
