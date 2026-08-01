"""Linux process discovery and cleanup for CLI UX observations."""

from __future__ import annotations

import os
import signal
import subprocess
import time
from pathlib import Path


TERM_GRACE_SECONDS = 0.15
KILL_GRACE_SECONDS = 1.0


def _signal_process_group(
    process: subprocess.Popen[bytes], number: int
) -> None:
    try:
        os.killpg(process.pid, number)
    except ProcessLookupError:
        pass


def _process_identity(pid: int) -> tuple[int, int] | None:
    try:
        stat_line = Path(f"/proc/{pid}/stat").read_text(encoding="ascii")
    except (FileNotFoundError, PermissionError, ProcessLookupError):
        return None
    close = stat_line.rfind(")")
    if close < 0:
        return None
    fields = stat_line[close + 2 :].split()
    if len(fields) < 4:
        return None
    return int(fields[1]), int(fields[3])


def _process_diagnostic(pid: int) -> str:
    try:
        stat_line = Path(f"/proc/{pid}/stat").read_text(encoding="ascii")
        close = stat_line.rfind(")")
        fields = stat_line[close + 2 :].split() if close >= 0 else []
        state = fields[0] if fields else "?"
        parent = fields[1] if len(fields) > 1 else "?"
        session = fields[3] if len(fields) > 3 else "?"
        command = (
            Path(f"/proc/{pid}/cmdline")
            .read_bytes()
            .replace(b"\0", b" ")
            .decode("utf-8", errors="replace")
            .strip()
        )
        cwd = os.readlink(f"/proc/{pid}/cwd")
        return (
            f"{pid} state={state} ppid={parent} session={session} "
            f"cwd={cwd} argv={command[:512]}"
        )
    except (FileNotFoundError, PermissionError, ProcessLookupError, OSError):
        return str(pid)


def _direct_child_pids() -> set[int]:
    parent = os.getpid()
    children_path = Path(f"/proc/{parent}/task/{parent}/children")
    try:
        return {
            int(value)
            for value in children_path.read_text(encoding="ascii").split()
        }
    except (FileNotFoundError, PermissionError, ProcessLookupError, ValueError):
        pass
    children: set[int] = set()
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        identity = _process_identity(int(entry.name))
        if identity is not None and identity[0] == parent:
            children.add(int(entry.name))
    return children


def _pids_in_capture_scope(
    session_id: int | None, baseline_children: set[int]
) -> list[int]:
    parent = os.getpid()
    new_children = _direct_child_pids() - baseline_children
    session_group_exists = False
    if session_id is not None:
        try:
            os.killpg(session_id, 0)
            session_group_exists = True
        except ProcessLookupError:
            pass
        except PermissionError:
            session_group_exists = True
    if not new_children and not session_group_exists:
        return []

    matches: set[int] = set(new_children)
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        pid = int(entry.name)
        if pid == parent:
            continue
        identity = _process_identity(pid)
        if identity is None:
            continue
        process_parent, process_session = identity
        if session_id is not None and process_session == session_id:
            matches.add(pid)
        if process_parent == parent and pid not in baseline_children:
            matches.add(pid)
    return sorted(matches)


def _reap_pid(pid: int) -> None:
    try:
        while True:
            waited, _ = os.waitpid(pid, os.WNOHANG)
            if waited == 0:
                return
            if waited == pid:
                return
    except ChildProcessError:
        return


def _reap_terminated_children(baseline_children: set[int]) -> None:
    for pid in _direct_child_pids() - baseline_children:
        _reap_pid(pid)


def _settle_capture_scope(
    session_id: int | None, baseline_children: set[int]
) -> list[int]:
    deadline = time.monotonic() + TERM_GRACE_SECONDS
    while True:
        _reap_terminated_children(baseline_children)
        scoped_pids = _pids_in_capture_scope(session_id, baseline_children)
        if not scoped_pids or time.monotonic() >= deadline:
            return scoped_pids
        time.sleep(0.01)


def _terminate_pids(pids: list[int]) -> list[int]:
    for pid in pids:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + TERM_GRACE_SECONDS
    remaining = list(pids)
    while remaining and time.monotonic() < deadline:
        time.sleep(0.01)
        for pid in remaining:
            _reap_pid(pid)
        remaining = [pid for pid in remaining if Path(f"/proc/{pid}").exists()]
    for pid in remaining:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + KILL_GRACE_SECONDS
    while remaining and time.monotonic() < deadline:
        time.sleep(0.01)
        for pid in remaining:
            _reap_pid(pid)
        remaining = [pid for pid in remaining if Path(f"/proc/{pid}").exists()]
    return remaining
