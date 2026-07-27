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
        SIDECAR_ROOT,
        SIDECAR_ROOT.parent,
    }
    if resolved in forbidden:
        raise HarnessError(f"refusing unsafe performance work base: {resolved}")
    resolved.mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(prefix="run-", dir=resolved)).resolve()


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
