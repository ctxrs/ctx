"""Representative source-family fixtures for the nightly performance gate."""

from __future__ import annotations

from dataclasses import dataclass
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import time
from typing import Callable
import unittest


FAMILY_LEAF_COUNT = 48
FAMILY_TEXT_BYTES = 384 * 1024
FAMILY_MIN_FIXTURE_BYTES = 16 * 1024 * 1024
FAMILY_MAX_FIXTURE_BYTES = 32 * 1024 * 1024

JSONL_FAMILY = "jsonl"
SQLITE_WAL_FAMILY = "sqlite_wal"
DOCUMENT_TREE_FAMILY = "replacement_document_tree"
EVENT_FILE_FAMILY = "event_file"
PROVIDER_SPECIFIC_EXCEPTION = "provider_specific_exception"

# The landed source-backed inventory has 42 provider identities. Codex's
# session and prompt-history formats share one JSONL gate; explicit leaf/tree
# variants remain in the same family. Custom is explicit-only and therefore
# keeps its provider-local gate instead of acquiring an automatic corpus.
PROVIDER_FAMILY_COVERAGE = (
    ("custom", PROVIDER_SPECIFIC_EXCEPTION, "explicit custom-history JSONL"),
    ("codex", JSONL_FAMILY, None),
    ("claude", JSONL_FAMILY, None),
    ("pi", JSONL_FAMILY, None),
    ("opencode", SQLITE_WAL_FAMILY, None),
    ("kilo", SQLITE_WAL_FAMILY, None),
    ("kiro_cli", SQLITE_WAL_FAMILY, None),
    ("antigravity", JSONL_FAMILY, None),
    ("gemini", JSONL_FAMILY, None),
    ("tabnine", JSONL_FAMILY, None),
    ("cursor", JSONL_FAMILY, None),
    ("windsurf", JSONL_FAMILY, None),
    ("zed", SQLITE_WAL_FAMILY, None),
    ("copilot_cli", JSONL_FAMILY, None),
    ("factory_ai_droid", JSONL_FAMILY, None),
    ("qwen_code", JSONL_FAMILY, None),
    ("kimi_code_cli", JSONL_FAMILY, None),
    ("auggie", DOCUMENT_TREE_FAMILY, None),
    ("junie", JSONL_FAMILY, None),
    ("firebender", SQLITE_WAL_FAMILY, None),
    ("forgecode", SQLITE_WAL_FAMILY, None),
    ("deepagents", SQLITE_WAL_FAMILY, None),
    ("mistral_vibe", JSONL_FAMILY, None),
    ("mux", JSONL_FAMILY, None),
    (
        "rovodev",
        DOCUMENT_TREE_FAMILY,
        "cross-leaf lineage is intentionally serial",
    ),
    ("openclaw", JSONL_FAMILY, None),
    ("hermes", SQLITE_WAL_FAMILY, None),
    ("nanoclaw", SQLITE_WAL_FAMILY, "compound central/component inventory"),
    ("astrbot", SQLITE_WAL_FAMILY, None),
    ("shelley", SQLITE_WAL_FAMILY, "exact-CWD single logical leaf"),
    ("continue", DOCUMENT_TREE_FAMILY, None),
    ("openhands", EVENT_FILE_FAMILY, None),
    ("cline", DOCUMENT_TREE_FAMILY, None),
    ("roo_code", DOCUMENT_TREE_FAMILY, None),
    ("crush", SQLITE_WAL_FAMILY, None),
    ("goose", SQLITE_WAL_FAMILY, None),
    ("lingma", SQLITE_WAL_FAMILY, None),
    ("qoder", JSONL_FAMILY, None),
    ("warp", SQLITE_WAL_FAMILY, None),
    ("codebuddy", DOCUMENT_TREE_FAMILY, None),
    ("trae", SQLITE_WAL_FAMILY, None),
    ("mimocode", SQLITE_WAL_FAMILY, None),
)


@dataclass
class FamilyCorpus:
    family: str
    provider: str
    source_format: str
    source_root: Path
    source_path_suffix: str
    cold_query: str
    replacement_query: str
    cold_body: str
    replacement_body: str
    source_count: int
    complete_records: int
    retained_records: int
    ignored_records: int
    rejected_records: int
    indexed_documents: int
    certified_source_bytes: int
    fixture_bytes: int
    independent_leaves: bool
    _replace_leaf: Callable[[], None]
    _close: Callable[[], None]

    def replace_leaf(self) -> None:
        self._replace_leaf()

    def close(self) -> None:
        self._close()


@dataclass(frozen=True)
class FamilyRefreshSample:
    packet: dict[str, object]
    elapsed_seconds: float
    cpu_seconds: float
    cpu_per_wall: float
    baseline_open_fds: int
    peak_open_fds: int
    peak_rss_bytes: int


def linux_process_cpu_seconds(pid: int) -> float:
    stat = (Path("/proc") / str(pid) / "stat").read_text(encoding="ascii")
    fields = stat.rsplit(")", 1)[1].split()
    clock_ticks = os.sysconf("SC_CLK_TCK")
    return (int(fields[11]) + int(fields[12])) / clock_ticks


def linux_peak_rss_bytes(pid: int) -> int:
    status_path = Path("/proc") / str(pid) / "status"
    values: dict[str, int] = {}
    for line in status_path.read_text(encoding="ascii").splitlines():
        name, separator, raw = line.partition(":")
        if separator and name in {"VmHWM", "VmRSS"}:
            parts = raw.split()
            if len(parts) == 2 and parts[1] == "kB":
                values[name] = int(parts[0]) * 1024
    return values.get("VmHWM", values.get("VmRSS", 0))


def linux_open_fd_count(pid: int) -> int:
    return len(tuple((Path("/proc") / str(pid) / "fd").iterdir()))


def run_family_refresh_measured(
    args: list[str],
    env: dict[str, str],
    cwd: Path,
    daemon_pid: int,
    timeout_seconds: float,
) -> FamilyRefreshSample:
    started = time.monotonic()
    initial_cpu = linux_process_cpu_seconds(daemon_pid)
    baseline_open_fds = linux_open_fd_count(daemon_pid)
    peak_open_fds = baseline_open_fds
    peak_rss_bytes = linux_peak_rss_bytes(daemon_pid)
    with tempfile.TemporaryFile(mode="w+b", dir=cwd) as stdout_file, (
        tempfile.TemporaryFile(mode="w+b", dir=cwd)
    ) as stderr_file:
        process = subprocess.Popen(
            [env["CTX_PERFORMANCE_TASK_BINARY"], *args],
            cwd=cwd,
            env=env,
            stdout=stdout_file,
            stderr=stderr_file,
        )
        deadline = started + timeout_seconds
        while process.poll() is None:
            peak_open_fds = max(peak_open_fds, linux_open_fd_count(daemon_pid))
            peak_rss_bytes = max(
                peak_rss_bytes, linux_peak_rss_bytes(daemon_pid)
            )
            if time.monotonic() >= deadline:
                process.kill()
                process.wait()
                stdout_file.seek(0)
                stderr_file.seek(0)
                raise TimeoutError(
                    f"{' '.join(args)} exceeded {timeout_seconds}s\n"
                    f"stdout:\n{stdout_file.read().decode(errors='replace')}\n"
                    f"stderr:\n{stderr_file.read().decode(errors='replace')}"
                )
            time.sleep(0.002)
        stdout_file.seek(0)
        stderr_file.seek(0)
        stdout = stdout_file.read()
        stderr = stderr_file.read()
    if process.returncode != 0:
        raise RuntimeError(
            f"{' '.join(args)} exited {process.returncode}\n"
            f"stdout:\n{stdout.decode(errors='replace')}\n"
            f"stderr:\n{stderr.decode(errors='replace')}"
        )
    packet = json.loads(stdout)
    if not isinstance(packet, dict):
        raise RuntimeError(f"{' '.join(args)} did not return a JSON object")
    elapsed_seconds = time.monotonic() - started
    cpu_seconds = linux_process_cpu_seconds(daemon_pid) - initial_cpu
    return FamilyRefreshSample(
        packet=packet,
        elapsed_seconds=elapsed_seconds,
        cpu_seconds=cpu_seconds,
        cpu_per_wall=cpu_seconds / elapsed_seconds,
        baseline_open_fds=baseline_open_fds,
        peak_open_fds=peak_open_fds,
        peak_rss_bytes=peak_rss_bytes,
    )


class SourceFamilyTaxonomyTest(unittest.TestCase):
    def test_every_landed_provider_has_one_family_or_exception(self) -> None:
        providers = [provider for provider, _, _ in PROVIDER_FAMILY_COVERAGE]
        self.assertEqual(len(providers), 42)
        self.assertEqual(len(set(providers)), 42)
        self.assertEqual(
            {family for _, family, _ in PROVIDER_FAMILY_COVERAGE},
            {
                JSONL_FAMILY,
                SQLITE_WAL_FAMILY,
                DOCUMENT_TREE_FAMILY,
                EVENT_FILE_FAMILY,
                PROVIDER_SPECIFIC_EXCEPTION,
            },
        )


def compact_json(value: object) -> bytes:
    return json.dumps(value, separators=(",", ":"), sort_keys=True).encode("utf-8")


def fixed_family_text(query: str) -> str:
    prefix = f"{query} "
    if len(prefix) >= FAMILY_TEXT_BYTES:
        raise ValueError("family fixture query exceeds its fixed body size")
    body = prefix + "x" * (FAMILY_TEXT_BYTES - len(prefix))
    if len(body.encode("ascii")) != FAMILY_TEXT_BYTES:
        raise AssertionError("family fixture body has the wrong byte count")
    return body


def directory_file_bytes(path: Path) -> int:
    return sum(entry.stat().st_size for entry in path.rglob("*") if entry.is_file())


def write_bytes(path: Path, body: bytes) -> int:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(body)
    return len(body)


def replace_bytes(path: Path, body: bytes) -> None:
    replacement = path.with_name(f"{path.name}.replacement")
    replacement.write_bytes(body)
    replacement.replace(path)


def family_query(family: str, leaf: int, phase: str = "cold") -> str:
    token = {
        SQLITE_WAL_FAMILY: "sqlitewal",
        DOCUMENT_TREE_FAMILY: "documenttree",
        EVENT_FILE_FAMILY: "eventfile",
    }[family]
    return f"ctxfamily{token}{phase}{leaf:03d}"


def write_lingma_sqlite_wal_corpus(home: Path) -> FamilyCorpus:
    connections: list[sqlite3.Connection] = []
    certified_source_bytes = 0
    first_path: Path | None = None
    cold_query = family_query(SQLITE_WAL_FAMILY, 0)
    replacement_query = family_query(SQLITE_WAL_FAMILY, 0, "repl")
    cold_body = fixed_family_text(cold_query)
    replacement_body = fixed_family_text(replacement_query)

    for leaf in range(FAMILY_LEAF_COUNT):
        selected_root = home / "lingma-profiles" / f"profile-{leaf:03d}"
        database_path = (
            selected_root / "sharedClientCache" / "cache" / "db" / "local.db"
        )
        database_path.parent.mkdir(parents=True, exist_ok=True)
        settings_path = (
            home
            / ".config"
            / "Code"
            / "User"
            / "profiles"
            / f"profile-{leaf:03d}"
            / "settings.json"
        )
        write_bytes(
            settings_path,
            compact_json({"Lingma.LocalMachineStoragePath": str(selected_root)}),
        )
        connection = sqlite3.connect(database_path)
        connection.execute(
            """
            create table chat_record (
                session_id text not null,
                request_id text,
                chat_prompt text,
                summary text,
                error_result text,
                gmt_create integer,
                extra text
            )
            """
        )
        connection.commit()
        journal_mode = connection.execute("pragma journal_mode = wal").fetchone()
        if journal_mode != ("wal",):
            raise RuntimeError(f"failed to create active Lingma WAL: {journal_mode!r}")
        connection.execute("pragma wal_autocheckpoint = 0")
        connection.execute("pragma synchronous = off")
        session_id = f"lingma-session-{leaf:03d}"
        request_id = f"lingma-request-{leaf:03d}"
        body = (
            cold_body
            if leaf == 0
            else fixed_family_text(family_query(SQLITE_WAL_FAMILY, leaf))
        )
        connection.execute(
            """
            insert into chat_record (
                session_id, request_id, chat_prompt, summary,
                error_result, gmt_create, extra
            ) values (?, ?, ?, null, null, 1785412800, null)
            """,
            (session_id, request_id, body),
        )
        connection.commit()
        wal_path = database_path.with_name("local.db-wal")
        shm_path = database_path.with_name("local.db-shm")
        if not wal_path.is_file() or not shm_path.is_file():
            raise RuntimeError("Lingma fixture did not retain committed WAL state")
        certified_source_bytes += (
            128
            + len(session_id.encode())
            + len(request_id.encode())
            + len(body.encode())
        )
        connections.append(connection)
        if first_path is None:
            first_path = database_path

    if first_path is None:
        raise AssertionError("Lingma family fixture requires at least one leaf")

    def replace_leaf() -> None:
        connections[0].execute(
            "update chat_record set chat_prompt = ? where request_id = ?",
            (replacement_body, "lingma-request-000"),
        )
        connections[0].commit()

    def close() -> None:
        for connection in connections:
            connection.close()

    return FamilyCorpus(
        family=SQLITE_WAL_FAMILY,
        provider="lingma",
        source_format="lingma_sqlite",
        source_root=home,
        source_path_suffix=str(first_path.relative_to(home)),
        cold_query=cold_query,
        replacement_query=replacement_query,
        cold_body=cold_body,
        replacement_body=replacement_body,
        source_count=FAMILY_LEAF_COUNT,
        complete_records=FAMILY_LEAF_COUNT,
        retained_records=FAMILY_LEAF_COUNT,
        ignored_records=0,
        rejected_records=0,
        indexed_documents=FAMILY_LEAF_COUNT,
        certified_source_bytes=certified_source_bytes,
        fixture_bytes=directory_file_bytes(home),
        independent_leaves=True,
        _replace_leaf=replace_leaf,
        _close=close,
    )


def cline_api_messages(message_id: str, body: str) -> bytes:
    return compact_json([{"content": body, "id": message_id, "role": "user"}])


def write_cline_document_tree_corpus(home: Path) -> FamilyCorpus:
    root = home / ".cline" / "data"
    certified_source_bytes = 0
    cold_query = family_query(DOCUMENT_TREE_FAMILY, 0)
    replacement_query = family_query(DOCUMENT_TREE_FAMILY, 0, "repl")
    cold_body = fixed_family_text(cold_query)
    replacement_body = fixed_family_text(replacement_query)
    first_api: Path | None = None

    for leaf in range(FAMILY_LEAF_COUNT):
        task_id = f"family-task-{leaf:03d}"
        task = root / "tasks" / task_id
        metadata = compact_json(
            {
                "createdAt": "2026-07-30T12:00:00Z",
                "task": f"source family task {leaf:03d}",
                "taskId": task_id,
                "workspaceDirectory": "/workspace/source-family",
            }
        )
        body = (
            cold_body
            if leaf == 0
            else fixed_family_text(family_query(DOCUMENT_TREE_FAMILY, leaf))
        )
        api = cline_api_messages(f"family-message-{leaf:03d}", body)
        certified_source_bytes += write_bytes(task / "task_metadata.json", metadata)
        certified_source_bytes += write_bytes(
            task / "api_conversation_history.json", api
        )
        if first_api is None:
            first_api = task / "api_conversation_history.json"

    if first_api is None:
        raise AssertionError("Cline family fixture requires at least one leaf")

    def replace_leaf() -> None:
        replace_bytes(
            first_api,
            cline_api_messages("family-message-000", replacement_body),
        )

    return FamilyCorpus(
        family=DOCUMENT_TREE_FAMILY,
        provider="cline",
        source_format="cline_task_directory_json",
        source_root=root,
        source_path_suffix="api_conversation_history.json",
        cold_query=cold_query,
        replacement_query=replacement_query,
        cold_body=cold_body,
        replacement_body=replacement_body,
        source_count=FAMILY_LEAF_COUNT,
        complete_records=FAMILY_LEAF_COUNT,
        retained_records=FAMILY_LEAF_COUNT,
        ignored_records=0,
        rejected_records=0,
        indexed_documents=FAMILY_LEAF_COUNT,
        certified_source_bytes=certified_source_bytes,
        fixture_bytes=directory_file_bytes(root),
        independent_leaves=True,
        _replace_leaf=replace_leaf,
        _close=lambda: None,
    )


def openhands_message(event_id: str, body: str) -> bytes:
    return compact_json(
        {
            "id": event_id,
            "kind": "MessageEvent",
            "llm_message": {"content": body, "role": "assistant"},
            "source": "agent",
            "timestamp": "2026-07-30T12:00:00Z",
        }
    )


def write_openhands_event_file_corpus(home: Path) -> FamilyCorpus:
    root = home / ".openhands"
    certified_source_bytes = 0
    cold_query = family_query(EVENT_FILE_FAMILY, 0)
    replacement_query = family_query(EVENT_FILE_FAMILY, 0, "repl")
    cold_body = fixed_family_text(cold_query)
    replacement_body = fixed_family_text(replacement_query)
    first_event: Path | None = None

    for leaf in range(FAMILY_LEAF_COUNT):
        event_id = f"family-openhands-event-{leaf:03d}"
        event = (
            root
            / "v1_conversations"
            / f"conversation-{leaf:03d}"
            / "0000-message.json"
        )
        body = (
            cold_body
            if leaf == 0
            else fixed_family_text(family_query(EVENT_FILE_FAMILY, leaf))
        )
        certified_source_bytes += write_bytes(
            event, openhands_message(event_id, body)
        )
        if first_event is None:
            first_event = event

    if first_event is None:
        raise AssertionError("OpenHands family fixture requires at least one leaf")

    def replace_leaf() -> None:
        replace_bytes(
            first_event,
            openhands_message("family-openhands-event-000", replacement_body),
        )

    return FamilyCorpus(
        family=EVENT_FILE_FAMILY,
        provider="openhands",
        source_format="openhands_file_events",
        source_root=root,
        source_path_suffix=str(first_event.relative_to(root)),
        cold_query=cold_query,
        replacement_query=replacement_query,
        cold_body=cold_body,
        replacement_body=replacement_body,
        source_count=FAMILY_LEAF_COUNT,
        complete_records=FAMILY_LEAF_COUNT,
        retained_records=FAMILY_LEAF_COUNT,
        ignored_records=0,
        rejected_records=0,
        indexed_documents=FAMILY_LEAF_COUNT,
        certified_source_bytes=certified_source_bytes,
        fixture_bytes=directory_file_bytes(root),
        independent_leaves=True,
        _replace_leaf=replace_leaf,
        _close=lambda: None,
    )


FAMILY_CORPUS_WRITERS = (
    write_lingma_sqlite_wal_corpus,
    write_cline_document_tree_corpus,
    write_openhands_event_file_corpus,
)
