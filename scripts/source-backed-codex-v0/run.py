#!/usr/bin/env python3
"""Run a source-backed Codex V0 end-to-end benchmark."""

from __future__ import annotations

import argparse
import collections
import datetime as dt
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import uuid
from pathlib import Path
from typing import Any


SCHEMA_VERSION = 1
TIME_FORMAT = "\n".join(
    (
        "wall_seconds=%e",
        "user_seconds=%U",
        "sys_seconds=%S",
        "max_rss_kib=%M",
        "exit_status=%x",
    )
)
SUPPORTED_CODEX_LOCATIONS = (
    ("sessions", "directory"),
    ("archived_sessions", "directory"),
    ("history.jsonl", "file"),
)


class HarnessError(RuntimeError):
    """A benchmark precondition or output contract failed."""


class PhaseError(HarnessError):
    """A measured CLI phase failed."""

    def __init__(self, phase: str, message: str):
        super().__init__(message)
        self.phase = phase


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Benchmark a candidate ctx binary against an immutable CODEX_HOME-compatible "
            "corpus using fresh, create-only data and output roots."
        )
    )
    parser.add_argument(
        "--candidate",
        required=True,
        type=Path,
        help="absolute path to the candidate ctx executable",
    )
    parser.add_argument(
        "--codex-home",
        required=True,
        type=Path,
        help=(
            "absolute path to an existing ordinary CODEX_HOME-compatible corpus root "
            "containing sessions/, archived_sessions/, or history.jsonl"
        ),
    )
    parser.add_argument(
        "--data-root",
        required=True,
        type=Path,
        help="absolute path for a fresh ctx data root; the path must not exist",
    )
    parser.add_argument("--query", required=True, help="non-empty lexical search query")
    parser.add_argument(
        "--output-dir",
        required=True,
        type=Path,
        help="absolute create-only benchmark output path; the path must not exist",
    )
    parser.add_argument(
        "--show-content",
        choices=("indexed", "complete"),
        default="complete",
        help="ctx show content policy (default: complete)",
    )
    parser.add_argument(
        "--sandbox",
        choices=("auto", "bwrap", "none"),
        default="auto",
        help=(
            "filesystem sandbox: auto probes bwrap and falls back to direct execution; "
            "bwrap requires a functional sandbox; none runs directly (default: auto)"
        ),
    )
    return parser.parse_args()


def require_absolute_canonical_existing(path: Path, label: str) -> Path:
    if not path.is_absolute():
        raise HarnessError(f"{label} must be an absolute path: {path}")
    normalized = Path(os.path.normpath(os.fspath(path)))
    if normalized != path:
        raise HarnessError(f"{label} must be normalized without '.' or '..': {path}")
    try:
        resolved = path.resolve(strict=True)
    except OSError as exc:
        raise HarnessError(f"{label} does not exist: {path}: {exc}") from exc
    if resolved != path:
        raise HarnessError(f"{label} or one of its parents is a symlink: {path}")
    return resolved


def require_fresh_path(path: Path, label: str) -> Path:
    if not path.is_absolute():
        raise HarnessError(f"{label} must be an absolute path: {path}")
    normalized = Path(os.path.normpath(os.fspath(path)))
    if normalized != path:
        raise HarnessError(f"{label} must be normalized without '.' or '..': {path}")
    if os.path.lexists(path):
        raise HarnessError(f"{label} must not already exist: {path}")
    parent = require_absolute_canonical_existing(path.parent, f"{label} parent")
    if not parent.is_dir() or parent.is_symlink():
        raise HarnessError(f"{label} parent is not an ordinary directory: {parent}")
    return parent / path.name


def paths_overlap(left: Path, right: Path) -> bool:
    return left == right or left in right.parents or right in left.parents


def probe_bwrap(bwrap: Path) -> dict[str, Any]:
    command = [
        os.fspath(bwrap),
        "--die-with-parent",
        "--ro-bind",
        "/",
        "/",
        "--",
        "/bin/true",
    ]
    try:
        completed = subprocess.run(
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            timeout=5,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return {
            "attempted": True,
            "functional": False,
            "exit_status": None,
            "diagnostic": str(exc)[:500],
        }
    diagnostic = completed.stderr.strip().replace("\n", " ")[:500]
    return {
        "attempted": True,
        "functional": completed.returncode == 0,
        "exit_status": completed.returncode,
        "diagnostic": diagnostic or None,
    }


def validate_inputs(args: argparse.Namespace) -> dict[str, Any]:
    candidate = require_absolute_canonical_existing(args.candidate, "candidate")
    candidate_stat = candidate.lstat()
    if not stat.S_ISREG(candidate_stat.st_mode) or candidate.is_symlink():
        raise HarnessError(f"candidate is not an ordinary regular file: {candidate}")
    if not os.access(candidate, os.X_OK):
        raise HarnessError(f"candidate is not executable: {candidate}")

    codex_home = require_absolute_canonical_existing(args.codex_home, "codex home")
    codex_stat = codex_home.lstat()
    if not stat.S_ISDIR(codex_stat.st_mode) or codex_home.is_symlink():
        raise HarnessError(f"codex home is not an ordinary directory: {codex_home}")

    locations: list[str] = []
    for name, expected_type in SUPPORTED_CODEX_LOCATIONS:
        location = codex_home / name
        if not os.path.lexists(location):
            continue
        resolved = require_absolute_canonical_existing(location, f"codex home {name}")
        valid = resolved.is_dir() if expected_type == "directory" else resolved.is_file()
        if not valid or resolved.is_symlink():
            raise HarnessError(
                f"codex home {name} is not an ordinary {expected_type}: {resolved}"
            )
        locations.append(name)
    if not locations:
        raise HarnessError(
            "codex home has no supported source: expected sessions/, "
            "archived_sessions/, or history.jsonl"
        )

    data_root = require_fresh_path(args.data_root, "data root")
    output_dir = require_fresh_path(args.output_dir, "output directory")
    if paths_overlap(data_root, output_dir):
        raise HarnessError("data root and output directory must be disjoint")
    if paths_overlap(codex_home, data_root) or paths_overlap(codex_home, output_dir):
        raise HarnessError("data root and output directory must be outside the source corpus")
    if not args.query.strip():
        raise HarnessError("query must not be empty")

    time_bin = Path("/usr/bin/time")
    if not time_bin.is_file() or not os.access(time_bin, os.X_OK):
        raise HarnessError("GNU /usr/bin/time is required")
    bwrap_raw = shutil.which("bwrap")
    bwrap = (
        require_absolute_canonical_existing(Path(bwrap_raw), "bwrap")
        if bwrap_raw is not None
        else None
    )
    if args.sandbox == "none":
        sandbox_mode = "none"
        sandbox_probe = {
            "attempted": False,
            "functional": None,
            "exit_status": None,
            "diagnostic": "disabled by --sandbox none",
        }
    elif bwrap is None:
        if args.sandbox == "bwrap":
            raise HarnessError("--sandbox bwrap requested but bwrap is not installed")
        sandbox_mode = "none"
        sandbox_probe = {
            "attempted": False,
            "functional": False,
            "exit_status": None,
            "diagnostic": "bwrap is not installed",
        }
    else:
        sandbox_probe = probe_bwrap(bwrap)
        if sandbox_probe["functional"]:
            sandbox_mode = "bwrap"
        elif args.sandbox == "bwrap":
            raise HarnessError(
                "--sandbox bwrap probe failed: "
                f"{sandbox_probe.get('diagnostic') or 'unknown error'}"
            )
        else:
            sandbox_mode = "none"

    return {
        "candidate": candidate,
        "codex_home": codex_home,
        "data_root": data_root,
        "output_dir": output_dir,
        "codex_locations": locations,
        "bwrap": bwrap,
        "sandbox_mode": sandbox_mode,
        "sandbox_probe": sandbox_probe,
        "sandbox_requested": args.sandbox,
        "time": time_bin,
    }


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def inventory_tree(root: Path, largest_limit: int = 12) -> dict[str, Any]:
    if not root.is_dir() or root.is_symlink():
        raise HarnessError(f"cannot inventory non-ordinary directory: {root}")

    counts: collections.Counter[str] = collections.Counter()
    logical_bytes = 0
    allocated_bytes = 0
    largest: list[tuple[int, str]] = []
    digest = hashlib.sha256()
    stack: list[tuple[Path, str]] = [(root, "")]
    counts["directories"] = 1

    while stack:
        directory, relative_directory = stack.pop()
        try:
            entries = sorted(os.scandir(directory), key=lambda entry: entry.name)
        except OSError as exc:
            raise HarnessError(f"inventory failed under {directory}: {exc}") from exc
        child_directories: list[tuple[Path, str]] = []
        for entry in entries:
            relative = (
                f"{relative_directory}/{entry.name}"
                if relative_directory
                else entry.name
            )
            try:
                metadata = entry.stat(follow_symlinks=False)
            except OSError as exc:
                raise HarnessError(f"inventory failed for {entry.path}: {exc}") from exc
            mode = metadata.st_mode
            if stat.S_ISREG(mode):
                kind = "file"
                counts["files"] += 1
                logical_bytes += metadata.st_size
                allocated_bytes += getattr(metadata, "st_blocks", 0) * 512
                largest.append((metadata.st_size, relative))
            elif stat.S_ISDIR(mode):
                kind = "directory"
                counts["directories"] += 1
                child_directories.append((Path(entry.path), relative))
            elif stat.S_ISLNK(mode):
                kind = "symlink"
                counts["symlinks"] += 1
            else:
                kind = "other"
                counts["other"] += 1
            digest.update(
                (
                    f"{kind}\0{relative}\0{stat.S_IMODE(mode):o}\0"
                    f"{metadata.st_size}\0{metadata.st_mtime_ns}\n"
                ).encode("utf-8", errors="surrogateescape")
            )
        stack.extend(reversed(child_directories))

    largest.sort(key=lambda item: (-item[0], item[1]))
    return {
        "file_count": counts["files"],
        "directory_count": counts["directories"],
        "symlink_count": counts["symlinks"],
        "other_count": counts["other"],
        "logical_bytes": logical_bytes,
        "allocated_bytes": allocated_bytes,
        "metadata_sha256": digest.hexdigest(),
        "largest_files": [
            {"relative_path": relative, "bytes": size}
            for size, relative in largest[:largest_limit]
        ],
    }


def artifact_descriptor(path: Path, output_dir: Path) -> dict[str, Any]:
    return {
        "path": path.relative_to(output_dir).as_posix(),
        "bytes": path.stat().st_size,
        "sha256": sha256_file(path),
    }


def parse_time_file(path: Path) -> dict[str, Any]:
    values: dict[str, str] = {}
    with path.open("r", encoding="utf-8") as handle:
        for raw in handle:
            key, separator, value = raw.strip().partition("=")
            if separator:
                values[key] = value
    required = (
        "wall_seconds",
        "user_seconds",
        "sys_seconds",
        "max_rss_kib",
        "exit_status",
    )
    missing = [key for key in required if key not in values]
    if missing:
        raise HarnessError(f"GNU time output is missing {', '.join(missing)}: {path}")
    try:
        return {
            "wall_seconds": float(values["wall_seconds"]),
            "user_seconds": float(values["user_seconds"]),
            "sys_seconds": float(values["sys_seconds"]),
            "max_rss_kib": int(values["max_rss_kib"]),
            "exit_status": int(values["exit_status"]),
        }
    except ValueError as exc:
        raise HarnessError(f"invalid GNU time output in {path}: {exc}") from exc


def stderr_report(path: Path) -> dict[str, Any]:
    line_count = 0
    json_line_count = 0
    non_json_line_count = 0
    types: collections.Counter[str] = collections.Counter()
    operations: collections.Counter[str] = collections.Counter()
    phases: collections.Counter[str] = collections.Counter()
    first_samples: list[Any] = []
    last_samples: collections.deque[Any] = collections.deque(maxlen=3)

    with path.open("r", encoding="utf-8", errors="replace") as handle:
        for raw in handle:
            line_count += 1
            text = raw.strip()
            if not text:
                continue
            try:
                value = json.loads(text)
            except json.JSONDecodeError:
                non_json_line_count += 1
                continue
            json_line_count += 1
            if isinstance(value, dict):
                for key, counter in (
                    ("type", types),
                    ("operation", operations),
                    ("phase", phases),
                ):
                    item = value.get(key)
                    if isinstance(item, str):
                        counter[item] += 1
                if len(first_samples) < 3:
                    first_samples.append(value)
                last_samples.append(value)

    return {
        "line_count": line_count,
        "json_line_count": json_line_count,
        "non_json_line_count": non_json_line_count,
        "types": dict(sorted(types.items())),
        "operations": dict(sorted(operations.items())),
        "phases": dict(sorted(phases.items())),
        "first_json_samples": first_samples,
        "last_json_samples": list(last_samples),
    }


def read_json(path: Path, phase: str) -> Any:
    try:
        with path.open("r", encoding="utf-8") as handle:
            return json.load(handle)
    except (OSError, json.JSONDecodeError) as exc:
        raise PhaseError(phase, f"{phase} stdout is not one JSON document: {exc}") from exc


def output_attribution(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        return {}
    selected: dict[str, Any] = {}
    for key in (
        "schema_version",
        "payload_type",
        "freshness",
        "retrieval",
        "phase_attribution",
        "phase_timings",
        "timings",
        "phases",
    ):
        if key in value:
            selected[key] = value[key]
    return selected


def valid_uuid(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    try:
        parsed = uuid.UUID(value)
    except ValueError:
        return None
    return str(parsed)


def extract_ids(search_result: Any, phase: str) -> tuple[str, str]:
    if not isinstance(search_result, dict) or not isinstance(
        search_result.get("results"), list
    ):
        raise PhaseError(phase, f"{phase} has no search results array")
    for result in search_result["results"]:
        if not isinstance(result, dict):
            continue
        event_id = valid_uuid(result.get("ctx_event_id")) or valid_uuid(
            result.get("event_id")
        )
        session_id = valid_uuid(result.get("ctx_session_id")) or valid_uuid(
            result.get("session_id")
        )
        if event_id and session_id:
            return event_id, session_id
    raise PhaseError(
        phase,
        f"{phase} returned no result containing both a ctx event ID and ctx session ID",
    )


class Runner:
    def __init__(
        self,
        paths: dict[str, Any],
        query: str,
        show_content: str,
        summary: dict[str, Any],
    ):
        self.candidate: Path = paths["candidate"]
        self.codex_home: Path = paths["codex_home"]
        self.data_root: Path = paths["data_root"]
        self.output_dir: Path = paths["output_dir"]
        self.bwrap: Path | None = paths["bwrap"]
        self.sandbox_mode: str = paths["sandbox_mode"]
        self.time: Path = paths["time"]
        self.query = query
        self.show_content = show_content
        self.summary = summary

        inherited_home = os.environ.get("HOME")
        self.environment = os.environ.copy()
        for name in (
            "CTX_DATA_ROOT",
            "CODEX_THREAD_ID",
            "CODEX_SESSION_ID",
            "RUST_LOG",
        ):
            self.environment.pop(name, None)
        self.environment.update(
            {
                "CODEX_HOME": os.fspath(self.codex_home),
                "CTX_ANALYTICS_ENABLED": "false",
                "CTX_LOCAL_USAGE_ENABLED": "false",
                "CTX_UPGRADE_AUTO": "off",
                "CTX_DAEMON_ENABLED": "false",
                "CTX_DAEMON_AUTOSTART_OFF": "1",
                "NO_COLOR": "1",
                "RUST_BACKTRACE": "0",
                "LC_ALL": "C.UTF-8",
                "TMPDIR": (
                    "/tmp"
                    if self.sandbox_mode == "bwrap"
                    else os.fspath(self.output_dir / "runtime/tmp")
                ),
                "XDG_CACHE_HOME": os.fspath(self.output_dir / "runtime/xdg-cache"),
                "XDG_CONFIG_HOME": os.fspath(self.output_dir / "runtime/xdg-config"),
                "XDG_DATA_HOME": os.fspath(self.output_dir / "runtime/xdg-data"),
                "XDG_STATE_HOME": os.fspath(self.output_dir / "runtime/xdg-state"),
                "CTX_BENCH_INHERITED_HOME": inherited_home or "",
            }
        )
        if self.environment.get("HOME") != inherited_home:
            raise HarnessError("internal error: HOME was repurposed")

    def sandbox_prefix(self) -> list[str]:
        if self.sandbox_mode == "none":
            return []
        if self.bwrap is None:
            raise HarnessError("internal error: bwrap mode has no bwrap executable")
        return [
            os.fspath(self.bwrap),
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--dev-bind",
            "/dev",
            "/dev",
            "--proc",
            "/proc",
            "--bind",
            os.fspath(self.data_root),
            os.fspath(self.data_root),
            "--bind",
            os.fspath(self.output_dir),
            os.fspath(self.output_dir),
            "--ro-bind",
            os.fspath(self.codex_home),
            os.fspath(self.codex_home),
            "--bind",
            os.fspath(self.output_dir / "runtime/tmp"),
            "/tmp",
            "--chdir",
            os.fspath(self.output_dir),
            "--",
        ]

    def run_phase(self, name: str, ctx_arguments: list[str]) -> Any:
        phase_dir = self.output_dir / "phases" / name
        phase_dir.mkdir(mode=0o700)
        stdout_path = phase_dir / "stdout.json"
        stderr_path = phase_dir / "stderr"
        time_path = phase_dir / "time.txt"
        exit_path = phase_dir / "exit-status.txt"

        candidate_argv = [os.fspath(self.candidate), *ctx_arguments]
        command = [
            *self.sandbox_prefix(),
            os.fspath(self.time),
            "--quiet",
            "--output",
            os.fspath(time_path),
            "--format",
            TIME_FORMAT,
            *candidate_argv,
        ]
        phase_summary: dict[str, Any] = {
            "name": name,
            "ctx_argv": candidate_argv,
            "started_at": utc_now(),
            "stdout": {"path": stdout_path.relative_to(self.output_dir).as_posix()},
            "stderr": {"path": stderr_path.relative_to(self.output_dir).as_posix()},
            "time": {"path": time_path.relative_to(self.output_dir).as_posix()},
        }
        self.summary["phases"].append(phase_summary)
        print(f"[source-backed-codex-v0] starting {name}", file=sys.stderr, flush=True)
        with stdout_path.open("wb") as stdout_handle, stderr_path.open(
            "wb"
        ) as stderr_handle:
            completed = subprocess.run(
                command,
                cwd=self.output_dir,
                env=self.environment,
                stdin=subprocess.DEVNULL,
                stdout=stdout_handle,
                stderr=stderr_handle,
                check=False,
            )
        exit_path.write_text(f"{completed.returncode}\n", encoding="ascii")
        phase_summary["finished_at"] = utc_now()
        phase_summary["process_exit_status"] = completed.returncode
        phase_summary["exit_status"] = artifact_descriptor(exit_path, self.output_dir)
        phase_summary["stdout"] = artifact_descriptor(stdout_path, self.output_dir)
        phase_summary["stderr"] = artifact_descriptor(stderr_path, self.output_dir)
        phase_summary["stderr_json"] = stderr_report(stderr_path)
        if time_path.exists():
            phase_summary["time"] = {
                **artifact_descriptor(time_path, self.output_dir),
                "metrics": parse_time_file(time_path),
            }

        if completed.returncode != 0:
            raise PhaseError(
                name,
                f"{name} exited {completed.returncode}; inspect "
                f"{stderr_path.relative_to(self.output_dir)}",
            )
        if not time_path.exists():
            raise PhaseError(name, f"{name} produced no GNU time receipt")
        if phase_summary["time"]["metrics"]["exit_status"] != 0:
            raise PhaseError(name, f"{name} GNU time receipt reports a nonzero exit")

        result = read_json(stdout_path, name)
        phase_summary["result_attribution"] = output_attribution(result)
        print(f"[source-backed-codex-v0] completed {name}", file=sys.stderr, flush=True)
        return result

    def search_arguments(self) -> list[str]:
        return [
            "--data-root",
            os.fspath(self.data_root),
            "search",
            "--provider",
            "codex",
            "--backend",
            "lexical",
            "--refresh",
            "wait",
            "--format",
            "json",
            self.query,
        ]

    def show_event_arguments(self, event_id: str) -> list[str]:
        return [
            "--data-root",
            os.fspath(self.data_root),
            "show",
            "event",
            event_id,
            "--content",
            self.show_content,
            "--format",
            "json",
        ]

    def show_session_arguments(self, session_id: str) -> list[str]:
        return [
            "--data-root",
            os.fspath(self.data_root),
            "show",
            "session",
            session_id,
            "--content",
            self.show_content,
            "--format",
            "json",
        ]


def write_summary(output_dir: Path, summary: dict[str, Any]) -> None:
    summary_path = output_dir / "summary.json"
    temporary_path = output_dir / ".summary.json.new"
    with temporary_path.open("x", encoding="utf-8") as handle:
        json.dump(summary, handle, indent=2, sort_keys=True)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary_path, summary_path)
    print(json.dumps(summary, sort_keys=True, separators=(",", ":")))


def main() -> int:
    args = parse_args()
    summary: dict[str, Any] | None = None
    paths: dict[str, Any] | None = None
    output_created = False
    return_code = 1

    try:
        paths = validate_inputs(args)
        output_dir: Path = paths["output_dir"]
        data_root: Path = paths["data_root"]
        output_dir.mkdir(mode=0o700)
        output_created = True
        data_root.mkdir(mode=0o700)
        for relative in (
            "phases",
            "runtime",
            "runtime/tmp",
            "runtime/xdg-cache",
            "runtime/xdg-config",
            "runtime/xdg-data",
            "runtime/xdg-state",
        ):
            (output_dir / relative).mkdir(mode=0o700)

        candidate_stat = paths["candidate"].stat()
        summary = {
            "schema_version": SCHEMA_VERSION,
            "benchmark": "source-backed-codex-v0",
            "status": "running",
            "started_at": utc_now(),
            "inputs": {
                "candidate": {
                    "path": os.fspath(paths["candidate"]),
                    "bytes": candidate_stat.st_size,
                    "sha256": sha256_file(paths["candidate"]),
                },
                "codex_home": os.fspath(paths["codex_home"]),
                "codex_locations": paths["codex_locations"],
                "data_root": os.fspath(data_root),
                "query": args.query,
                "output_dir": os.fspath(output_dir),
                "show_content": args.show_content,
            },
            "safety": {
                "create_only_paths": True,
                "sandbox_requested": paths["sandbox_requested"],
                "sandbox_mode": paths["sandbox_mode"],
                "sandbox_probe": paths["sandbox_probe"],
                "corpus_protection": (
                    "read_only_bubblewrap_bind"
                    if paths["sandbox_mode"] == "bwrap"
                    else "direct_execution_with_before_after_full_inventory_assertion"
                ),
                "host_root_mount": (
                    "read_only_bubblewrap_bind"
                    if paths["sandbox_mode"] == "bwrap"
                    else "not_sandboxed"
                ),
                "writable_paths": [
                    os.fspath(data_root),
                    os.fspath(output_dir),
                    (
                        f"{output_dir}/runtime/tmp mounted at /tmp"
                        if paths["sandbox_mode"] == "bwrap"
                        else f"{output_dir}/runtime/tmp used as TMPDIR"
                    ),
                ],
                "network": (
                    "inherited; analytics, local usage, daemon startup, semantic search, "
                    "and auto-upgrade disabled"
                ),
                "home_repurposed": False,
            },
            "inventories": {},
            "selected_ids": {},
            "phases": [],
        }
        source_before = inventory_tree(paths["codex_home"])
        summary["inventories"]["source_before"] = source_before

        runner = Runner(paths, args.query, args.show_content, summary)
        cold_search = runner.run_phase("01-query-cold", runner.search_arguments())
        event_id, session_id = extract_ids(cold_search, "01-query-cold")
        summary["selected_ids"] = {
            "ctx_event_id": event_id,
            "ctx_session_id": session_id,
            "source_phase": "01-query-cold",
        }
        summary["inventories"]["ctx_data_root_after_cold_query"] = inventory_tree(
            data_root
        )

        runner.run_phase(
            "02-show-event-cold", runner.show_event_arguments(event_id)
        )
        runner.run_phase(
            "03-show-session-cold", runner.show_session_arguments(session_id)
        )
        warm_search = runner.run_phase("04-query-warm", runner.search_arguments())
        warm_event_id, warm_session_id = extract_ids(warm_search, "04-query-warm")
        summary["selected_ids"]["warm_query_first_ctx_event_id"] = warm_event_id
        summary["selected_ids"]["warm_query_first_ctx_session_id"] = warm_session_id
        summary["inventories"]["ctx_data_root_after_warm_query"] = inventory_tree(
            data_root
        )
        runner.run_phase(
            "05-show-event-warm", runner.show_event_arguments(event_id)
        )
        runner.run_phase(
            "06-show-session-warm", runner.show_session_arguments(session_id)
        )

        source_after = inventory_tree(paths["codex_home"])
        summary["inventories"]["source_after"] = source_after
        summary["inventories"]["source_unchanged"] = source_after == source_before
        if source_after != source_before:
            raise HarnessError("source corpus inventory changed despite read-only isolation")
        summary["inventories"]["ctx_data_root_final"] = inventory_tree(data_root)
        summary["status"] = "success"
        return_code = 0
    except KeyboardInterrupt:
        if summary is not None:
            summary["status"] = "failed"
            summary["error"] = {"type": "KeyboardInterrupt", "message": "interrupted"}
        return_code = 130
    except Exception as exc:  # The summary is the durable failure handoff.
        if summary is not None:
            summary["status"] = "failed"
            error: dict[str, Any] = {
                "type": type(exc).__name__,
                "message": str(exc),
            }
            if isinstance(exc, PhaseError):
                error["failed_phase"] = exc.phase
            summary["error"] = error
        else:
            print(f"source-backed-codex-v0: {exc}", file=sys.stderr)
        return_code = 1
    finally:
        if summary is not None and paths is not None:
            try:
                if "source_after" not in summary["inventories"]:
                    source_after = inventory_tree(paths["codex_home"])
                    summary["inventories"]["source_after"] = source_after
                    summary["inventories"]["source_unchanged"] = (
                        source_after == summary["inventories"].get("source_before")
                    )
                if "ctx_data_root_final" not in summary["inventories"]:
                    summary["inventories"]["ctx_data_root_final"] = inventory_tree(
                        paths["data_root"]
                    )
            except Exception as inventory_error:
                summary.setdefault(
                    "error",
                    {
                        "type": type(inventory_error).__name__,
                        "message": str(inventory_error),
                    },
                )
                summary["status"] = "failed"
                return_code = 1
            summary["finished_at"] = utc_now()
            write_summary(paths["output_dir"], summary)
        elif output_created and paths is not None:
            print(
                f"source-backed-codex-v0: failed before summary creation; "
                f"output retained at {paths['output_dir']}",
                file=sys.stderr,
            )
    return return_code


if __name__ == "__main__":
    raise SystemExit(main())
