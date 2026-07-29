#!/usr/bin/env bash

perf_smoke_emit_python_fixtures() {
  cat <<'PY'
def prepare_work_root(base: Path) -> Path:
    resolved = base.resolve()
    forbidden = {
        Path("/").resolve(),
        Path.home().resolve(),
        REPO_ROOT,
        REPO_ROOT.parent,
        HARNESS_ROOT,
        HARNESS_ROOT.parent,
    }
    if resolved in forbidden:
        raise HarnessError(f"refusing unsafe performance work base: {resolved}")
    resolved.mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(prefix="run-", dir=resolved)).resolve()


PRIOR_EPOCH_SENTINEL = b"opaque ctx v0.25 rollback sentinel; never open as sqlite\n"


def stage_daemon_binary(ctx_bin: Path, role: str, work_root: Path) -> Path:
    metadata = ctx_bin.lstat()
    owner_safe = (
        stat.S_ISREG(metadata.st_mode)
        and not ctx_bin.is_symlink()
        and metadata.st_uid == os.getuid()
        and metadata.st_nlink == 1
        and metadata.st_mode & 0o022 == 0
    )
    if owner_safe:
        return ctx_bin
    staged = work_root / "binaries" / role / "ctx"
    staged.parent.mkdir(parents=True, exist_ok=False)
    shutil.copyfile(ctx_bin, staged)
    staged.chmod(0o700)
    if binary_sha256(staged) != binary_sha256(ctx_bin):
        raise HarnessError(f"staged {role} binary bytes differ from {ctx_bin}")
    return staged


def stage_run_specs(
    run_specs: list[tuple[Path, str, str]],
    work_root: Path,
) -> list[tuple[Path, str, str]]:
    return [
        (stage_daemon_binary(ctx_bin, role, work_root), label, role)
        for ctx_bin, label, role in run_specs
    ]


def install_prior_epoch_sentinel(data_root: Path) -> str:
    path = data_root / "work.sqlite"
    if path.exists():
        raise HarnessError(f"prior-epoch sentinel path already exists: {path}")
    path.write_bytes(PRIOR_EPOCH_SENTINEL)
    return hashlib.sha256(PRIOR_EPOCH_SENTINEL).hexdigest()


def assert_prior_epoch_sentinel(data_root: Path, expected_sha256: str) -> None:
    path = data_root / "work.sqlite"
    if not path.is_file():
        raise HarnessError(f"prior-epoch rollback sentinel disappeared: {path}")
    observed = hashlib.sha256(path.read_bytes()).hexdigest()
    if observed != expected_sha256:
        raise HarnessError(
            f"prior-epoch rollback sentinel changed: {observed} != {expected_sha256}"
        )
    for suffix in ("-wal", "-shm", "-journal"):
        auxiliary = Path(f"{path}{suffix}")
        if auxiliary.exists():
            raise HarnessError(f"source-backed refresh touched prior-epoch auxiliary: {auxiliary}")


def json_line(value: object) -> str:
    return json.dumps(value, separators=(",", ":"), sort_keys=True) + "\n"


def timestamp(index: int, event_index: int) -> str:
    base = dt.datetime(2026, 6, 26, tzinfo=dt.timezone.utc)
    instant = base + dt.timedelta(seconds=index % 86_400, milliseconds=event_index)
    return instant.strftime("%Y-%m-%dT%H:%M:%S.") + f"{instant.microsecond // 1000:03d}Z"


def session_path(corpus_root: Path, index: int) -> Path:
    shard = f"{index // 1000:02d}"
    return corpus_root / "2026" / "06" / "26" / shard / f"synthetic-session-{index:06d}.jsonl"


def generated_lines(
    index: int,
    marker: str,
    session_variant: str | None = None,
    event_count: int = 3,
) -> list[str]:
    if event_count < 3:
        raise HarnessError(f"event_count must be at least 3, got {event_count}")
    session_id = f"synthetic-codex-session-{index:06d}"
    if session_variant is not None:
        session_id = f"{session_id}-{session_variant}"
    cwd = "/workspace/ctx"
    lines = [
        json_line(
            {
                "timestamp": timestamp(index, 0),
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "timestamp": timestamp(index, 0),
                    "cwd": cwd,
                    "originator": "codex-cli",
                    "cli_version": "0.2.0-perf-smoke",
                    "source": "cli",
                    "model_provider": "openai",
                },
            }
        ),
        json_line(
            {
                "timestamp": timestamp(index, 1),
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [
                        {
                            "type": "input_text",
                            "text": f"{QUERY} generated ctx perf smoke corpus session {index:06d} {marker}",
                        }
                    ],
                },
            }
        ),
        json_line(
            {
                "timestamp": timestamp(index, 2),
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": f"Indexing deterministic performance fixture {index:06d}.",
                        }
                    ],
                    "phase": "commentary",
                },
            }
        ),
        json_line(
            {
                "timestamp": timestamp(index, 3),
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "exec_command",
                    "arguments": json.dumps(
                        {
                            "cmd": f"cargo test -p ctx synthetic_perf_{index:06d}",
                            "workdir": cwd,
                            "yield_time_ms": 1000,
                        },
                        separators=(",", ":"),
                    ),
                    "call_id": f"call-perf-{index:06d}",
                },
            }
        ),
        json_line(
            {
                "timestamp": timestamp(index, event_count + 1),
                "type": "event_msg",
                "payload": {
                    "type": "task_complete",
                    "last_agent_message": f"{QUERY} completed generated fixture session {index:06d}.",
                },
            }
        ),
    ]
    extra_messages = [
        json_line(
            {
                "timestamp": timestamp(index, 4 + extra_index),
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [
                        {
                            "type": "input_text",
                            "text": (
                                f"{QUERY} bounded multi-batch event {extra_index:06d} "
                                f"session {index:06d} {marker}"
                            ),
                        }
                    ],
                },
            }
        )
        for extra_index in range(event_count - 3)
    ]
    lines[-1:-1] = extra_messages
    return lines


def generate_corpus(
    corpus_root: Path,
    sessions: int,
    large_session_events: int,
) -> tuple[int, int]:
    bytes_written = 0
    events = 0
    for index in range(sessions):
        path = session_path(corpus_root, index)
        path.parent.mkdir(parents=True, exist_ok=True)
        event_count = large_session_events if index == 0 else 3
        body = "".join(generated_lines(index, "baseline", event_count=event_count))
        path.write_text(body, encoding="utf-8")
        bytes_written += len(body.encode("utf-8"))
        events += event_count
    return bytes_written, events


def append_changed_events(
    corpus_root: Path,
    sessions: int,
    changed_files: int,
    sample: int,
    large_session_events: int,
) -> None:
    for offset in range(changed_files):
        index = (sample * changed_files + offset) % sessions
        path = session_path(corpus_root, index)
        line = json_line(
            {
                "timestamp": timestamp(index, large_session_events + 100 + sample),
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [
                        {
                            "type": "input_text",
                            "text": (
                                f"{QUERY} changed incremental import sample {sample:02d} "
                                f"file {offset:02d} session {index:06d}"
                            ),
                        }
                    ],
                },
            }
        )
        with path.open("a", encoding="utf-8") as handle:
            handle.write(line)


def replace_changed_sessions(
    corpus_root: Path,
    sessions: int,
    changed_files: int,
    sample: int,
    large_session_events: int,
) -> int:
    replacement_events = 0
    for offset in range(changed_files):
        index = (sample * changed_files + offset) % sessions
        path = session_path(corpus_root, index)
        variant = f"replacement-{sample:02d}"
        event_count = large_session_events if index == 0 else 3
        body = "".join(
            generated_lines(
                index,
                f"replacement sample {sample:02d}",
                variant,
                event_count,
            )
        )
        path.write_text(body, encoding="utf-8")
        changed_at = time.time_ns() + offset
        os.utime(path, ns=(changed_at, changed_at))
        replacement_events += event_count
    return replacement_events


def delete_sessions(
    corpus_root: Path,
    max_files: int,
) -> tuple[int, int]:
    source_paths = sorted(corpus_root.rglob("*.jsonl"))
    deleted_sessions = min(max_files, len(source_paths) - 1)
    if deleted_sessions <= 0:
        raise HarnessError("delete profile requires at least two provider source files")
    deleted_events = 0
    for path in source_paths[-deleted_sessions:]:
        lines = path.read_text(encoding="utf-8").splitlines()
        deleted_events += len(lines) - 2
        path.unlink()
    return deleted_sessions, deleted_events


def corpus_counts(corpus_root: Path) -> tuple[int, int]:
    source_paths = sorted(corpus_root.rglob("*.jsonl"))
    events = 0
    for path in source_paths:
        lines = path.read_text(encoding="utf-8").splitlines()
        if len(lines) < 3:
            raise HarnessError(f"generated source has too few records: {path}")
        events += len(lines) - 2
    return len(source_paths), events


def command_env(home: Path, data_root: Path, temp_root: Path) -> dict[str, str]:
    env = os.environ.copy()
    env.update(
        {
            "HOME": str(home),
            "CODEX_HOME": str(home / ".codex"),
            "CTX_DATA_ROOT": str(data_root),
            "CTX_ANALYTICS_ENABLED": "false",
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


PY
}
