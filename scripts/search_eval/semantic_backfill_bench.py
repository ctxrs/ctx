#!/usr/bin/env python3
"""Benchmark standalone semantic backfill over a ctx work.sqlite store.

This is intentionally separate from the product path so we can time real
embedding/model cost against a fresh lexical import without changing work.sqlite.
It writes normalized float32 vectors into a sidecar SQLite DB.

Use semantic_worker_bench.py when measuring the product search + background
worker lifecycle.
"""

from __future__ import annotations

import argparse
import array
from dataclasses import dataclass
import hashlib
import json
import math
import os
import pathlib
import sqlite3
import sys
import time


EMBEDDING_MODE_EVENT = "event"
EMBEDDING_MODE_CHUNKED = "chunked"
SOURCE_MODE_EVENT_SEARCH_PREVIEW = "event_search_preview"
SOURCE_MODE_SEMANTIC_PAYLOAD = "semantic_payload"
SOURCE_TEXT_MAX_CHARS = 64 * 1024
DEFAULT_MODEL = "intfloat/multilingual-e5-small"
DEFAULT_MODEL_KEY = (
    "fastembed:intfloat-multilingual-e5-small:"
    "e5-query-passage:semantic-lite-turn-1200-200-v3"
)
DEFAULT_DIMENSIONS = 384
E5_QUERY_PREFIX = "query: "
E5_PASSAGE_PREFIX = "passage: "


@dataclass(frozen=True)
class SourceDocument:
    event_id: str
    event_seq: int
    text: str


@dataclass(frozen=True)
class TextChunk:
    event_id: str
    event_seq: int
    source_text_sha256: str
    source_char_len: int
    chunk_index: int
    chunk_count: int
    chunk_start_char: int
    chunk_end_char: int
    text: str


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def semantic_prefixed_text(prefix: str, text: str) -> str:
    text = text.lstrip()
    return text if text.startswith(prefix) else f"{prefix}{text}"


def semantic_query_text(text: str) -> str:
    return semantic_prefixed_text(E5_QUERY_PREFIX, text)


def semantic_passage_text(text: str) -> str:
    return semantic_prefixed_text(E5_PASSAGE_PREFIX, text)


def vector_blob(value) -> bytes:
    vector = [float(item) for item in value]
    norm = math.sqrt(sum(item * item for item in vector))
    if norm > 0:
        vector = [item / norm for item in vector]
    blob = array.array("f", vector)
    if blob.itemsize != 4:
        raise RuntimeError(f"expected 32-bit float array, got {blob.itemsize * 8}-bit")
    if sys.byteorder != "little":
        blob.byteswap()
    return blob.tobytes()


def sidecar_file_bytes(path: str) -> int:
    total = 0
    for suffix in ("", "-wal", "-shm"):
        candidate = path + suffix
        if os.path.exists(candidate):
            total += os.path.getsize(candidate)
    return total


def table_columns(conn, table_name: str) -> set[str]:
    return {row[1] for row in conn.execute(f"PRAGMA table_info({table_name})")}


def local_preview(value: str, max_chars: int) -> str:
    return value[:max_chars]


def non_blank(value) -> str | None:
    if not isinstance(value, str):
        return None
    trimmed = value.strip()
    return trimmed or None


def event_preview_fragment(value) -> str | None:
    if isinstance(value, str):
        return non_blank(value)
    if isinstance(value, bool) or isinstance(value, int) or isinstance(value, float):
        return str(value)
    return None


def event_value_preview(value) -> str | None:
    if isinstance(value, str):
        return non_blank(value)
    if not isinstance(value, dict):
        return None
    for key in (
        "text",
        "preview",
        "summary",
        "command",
        "output_preview",
        "output",
        "message",
    ):
        fragment = event_preview_fragment(value.get(key))
        if fragment:
            return fragment
    structured = []
    for key in ("tool", "name", "arguments_preview", "status"):
        fragment = event_preview_fragment(value.get(key))
        if fragment:
            structured.append(f"{key}: {fragment}")
    return " | ".join(structured) if structured else None


def event_payload_preview(payload) -> str | None:
    if isinstance(payload, dict) and "body" in payload:
        preview = event_value_preview(payload["body"])
        if preview:
            return preview
    return event_value_preview(payload)


def semantic_payload_text(payload_json: str, redaction_state: str) -> str:
    if redaction_state in ("raw", "withheld"):
        return "raw event payload withheld"
    payload = json.loads(payload_json)
    preview = event_payload_preview(payload)
    if preview is None:
        if isinstance(payload, (dict, list)):
            preview = json.dumps(payload, separators=(",", ":"))
        else:
            preview = ""
    return local_preview(preview, SOURCE_TEXT_MAX_CHARS)


def source_document_from_row(row, source_mode: str) -> SourceDocument:
    if source_mode == SOURCE_MODE_EVENT_SEARCH_PREVIEW:
        return SourceDocument(event_id=str(row[0]), event_seq=int(row[1]), text=row[2] or "")
    if source_mode == SOURCE_MODE_SEMANTIC_PAYLOAD:
        return SourceDocument(
            event_id=str(row[0]),
            event_seq=int(row[1]),
            text=semantic_payload_text(row[2] or "{}", row[3] or "safe_preview"),
        )
    raise ValueError(f"unsupported source mode: {source_mode}")


def setup_sidecar(path: str):
    conn = sqlite3.connect(path)
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA synchronous=NORMAL")
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS embedding_models (
            model_key TEXT PRIMARY KEY,
            model_id TEXT NOT NULL,
            dimensions INTEGER NOT NULL,
            created_at_ms INTEGER NOT NULL
        )
        """
    )
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS event_embeddings (
            event_id TEXT NOT NULL,
            model_key TEXT NOT NULL,
            source_mode TEXT NOT NULL DEFAULT 'event_search_preview',
            event_seq INTEGER NOT NULL,
            text_sha256 TEXT NOT NULL,
            dimensions INTEGER NOT NULL,
            embedding_f32 BLOB NOT NULL,
            embedded_at_ms INTEGER NOT NULL,
            PRIMARY KEY (event_id, model_key)
        )
        """
    )
    if "source_mode" not in table_columns(conn, "event_embeddings"):
        conn.execute(
            """
            ALTER TABLE event_embeddings
            ADD COLUMN source_mode TEXT NOT NULL DEFAULT 'event_search_preview'
            """
        )
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_event_embeddings_model_seq ON event_embeddings(model_key, event_seq)"
    )
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS event_chunk_embeddings (
            event_id TEXT NOT NULL,
            model_key TEXT NOT NULL,
            event_seq INTEGER NOT NULL,
            source_mode TEXT NOT NULL,
            source_text_sha256 TEXT NOT NULL,
            source_char_len INTEGER NOT NULL,
            chunk_index INTEGER NOT NULL,
            chunk_count INTEGER NOT NULL,
            chunk_start_char INTEGER NOT NULL,
            chunk_end_char INTEGER NOT NULL,
            chunk_char_len INTEGER NOT NULL,
            chunk_text_sha256 TEXT NOT NULL,
            chunk_target_chars INTEGER NOT NULL,
            chunk_overlap_chars INTEGER NOT NULL,
            dimensions INTEGER NOT NULL,
            embedding_f32 BLOB NOT NULL,
            embedded_at_ms INTEGER NOT NULL,
            PRIMARY KEY (
                event_id,
                model_key,
                source_mode,
                chunk_target_chars,
                chunk_overlap_chars,
                chunk_index
            )
        )
        """
    )
    conn.execute(
        """
        CREATE INDEX IF NOT EXISTS idx_event_chunk_embeddings_model_seq
        ON event_chunk_embeddings(
            model_key,
            source_mode,
            chunk_target_chars,
            chunk_overlap_chars,
            event_seq,
            chunk_index
        )
        """
    )
    conn.execute(
        """
        CREATE INDEX IF NOT EXISTS idx_event_chunk_embeddings_event_model
        ON event_chunk_embeddings(event_id, model_key)
        """
    )
    conn.commit()
    return conn


def fetch_batch(conn, source_mode, last_seq, limit) -> list[SourceDocument]:
    if source_mode == SOURCE_MODE_EVENT_SEARCH_PREVIEW:
        rows = conn.execute(
            """
            SELECT event_search.event_id, e.seq, event_search.safe_preview_text
            FROM event_search
            JOIN events e ON e.id = event_search.event_id
            WHERE e.deleted_at_ms IS NULL
              AND length(trim(event_search.safe_preview_text)) > 0
              AND (? IS NULL OR e.seq > ?)
            ORDER BY e.seq
            LIMIT ?
            """,
            (last_seq, last_seq, limit),
        ).fetchall()
        return [source_document_from_row(row, source_mode) for row in rows]
    if source_mode == SOURCE_MODE_SEMANTIC_PAYLOAD:
        rows = conn.execute(
            """
            SELECT event_search.event_id, e.seq, e.payload_json, e.redaction_state
            FROM event_search
            JOIN events e ON e.id = event_search.event_id
            WHERE e.deleted_at_ms IS NULL
              AND e.visibility != 'withheld'
              AND e.sync_state != 'withheld'
              AND e.redaction_state NOT IN ('raw', 'withheld')
              AND length(trim(event_search.safe_preview_text)) > 0
              AND (? IS NULL OR e.seq > ?)
            ORDER BY e.seq
            LIMIT ?
            """,
            (last_seq, last_seq, limit),
        ).fetchall()
        return [source_document_from_row(row, source_mode) for row in rows]
    raise ValueError(f"unsupported source mode: {source_mode}")


def quote_sql_string(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def source_select_columns(source_mode: str) -> str:
    if source_mode == SOURCE_MODE_EVENT_SEARCH_PREVIEW:
        return "event_search.event_id, e.seq, event_search.safe_preview_text"
    if source_mode == SOURCE_MODE_SEMANTIC_PAYLOAD:
        return "event_search.event_id, e.seq, e.payload_json, e.redaction_state"
    raise ValueError(f"unsupported source mode: {source_mode}")


def source_where_filters(source_mode: str) -> str:
    if source_mode == SOURCE_MODE_EVENT_SEARCH_PREVIEW:
        return ""
    if source_mode == SOURCE_MODE_SEMANTIC_PAYLOAD:
        return """
              AND e.visibility != 'withheld'
              AND e.sync_state != 'withheld'
              AND e.redaction_state NOT IN ('raw', 'withheld')
        """
    raise ValueError(f"unsupported source mode: {source_mode}")

def missing_event_sql(embedding_mode: str, source_mode: str) -> str:
    source_columns = source_select_columns(source_mode)
    source_filters = source_where_filters(source_mode)
    if embedding_mode == EMBEDDING_MODE_EVENT:
        return f"""
            SELECT {source_columns}
            FROM event_search
            JOIN events e ON e.id = event_search.event_id
            LEFT JOIN vector_sidecar.event_embeddings ve
              ON ve.event_id = event_search.event_id
             AND ve.model_key = ?
             AND ve.source_mode = ?
            WHERE e.deleted_at_ms IS NULL
              AND length(trim(event_search.safe_preview_text)) > 0
              {source_filters}
              AND ve.event_id IS NULL
            ORDER BY e.seq
        """
    if embedding_mode == EMBEDDING_MODE_CHUNKED:
        return f"""
            SELECT {source_columns}
            FROM event_search
            JOIN events e ON e.id = event_search.event_id
            LEFT JOIN (
                SELECT event_id
                FROM vector_sidecar.event_chunk_embeddings
                WHERE model_key = ?
                  AND source_mode = ?
                  AND chunk_target_chars = ?
                  AND chunk_overlap_chars = ?
                GROUP BY event_id
            ) ce ON ce.event_id = event_search.event_id
            WHERE e.deleted_at_ms IS NULL
              AND length(trim(event_search.safe_preview_text)) > 0
              {source_filters}
              AND ce.event_id IS NULL
            ORDER BY e.seq
        """
    raise ValueError(f"unsupported embedding mode: {embedding_mode}")


def fetch_missing_from_sidecar(
    conn,
    sidecar_path,
    model_key,
    limit,
    embedding_mode,
    source_mode,
    chunk_target_chars,
    chunk_overlap_chars,
) -> list[SourceDocument]:
    conn.execute(f"ATTACH DATABASE {quote_sql_string(sidecar_path)} AS vector_sidecar")
    try:
        sql = missing_event_sql(embedding_mode, source_mode)
        if embedding_mode == EMBEDDING_MODE_EVENT:
            params = [model_key, source_mode]
        elif embedding_mode == EMBEDDING_MODE_CHUNKED:
            params = [model_key]
            params.extend([source_mode, chunk_target_chars, chunk_overlap_chars])
        else:
            raise ValueError(f"unsupported embedding mode: {embedding_mode}")
        if limit is not None:
            sql += " LIMIT ?"
            params.append(limit)
        rows = conn.execute(sql, params).fetchall()
        return [source_document_from_row(row, source_mode) for row in rows]
    finally:
        conn.execute("DETACH DATABASE vector_sidecar")


def chunk_document(
    document: SourceDocument,
    target_chars: int,
    overlap_chars: int,
) -> list[TextChunk]:
    if target_chars <= 0:
        raise ValueError("--chunk-target-chars must be greater than 0")
    if overlap_chars < 0:
        raise ValueError("--chunk-overlap-chars must be at least 0")
    if overlap_chars >= target_chars:
        raise ValueError("--chunk-overlap-chars must be less than --chunk-target-chars")

    source_len = len(document.text)
    if source_len == 0:
        return []

    ranges: list[tuple[int, int]] = []
    start = 0
    while start < source_len:
        end = min(source_len, start + target_chars)
        ranges.append((start, end))
        if end >= source_len:
            break
        start = end - overlap_chars

    source_hash = sha256_text(document.text)
    chunk_count = len(ranges)
    return [
        TextChunk(
            event_id=document.event_id,
            event_seq=document.event_seq,
            source_text_sha256=source_hash,
            source_char_len=source_len,
            chunk_index=index,
            chunk_count=chunk_count,
            chunk_start_char=start,
            chunk_end_char=end,
            text=document.text[start:end],
        )
        for index, (start, end) in enumerate(ranges)
    ]


def event_records(
    docs: list[SourceDocument],
    embeddings,
    model_key,
    source_mode,
    dimensions,
    now_ms,
):
    return [
        (
            doc.event_id,
            model_key,
            source_mode,
            doc.event_seq,
            sha256_text(doc.text),
            dimensions,
            vector_blob(embedding),
            now_ms,
        )
        for doc, embedding in zip(docs, embeddings)
    ]


def chunk_records(
    chunks: list[TextChunk],
    embeddings,
    model_key,
    source_mode,
    chunk_target_chars,
    chunk_overlap_chars,
    dimensions,
    now_ms,
):
    return [
        (
            chunk.event_id,
            model_key,
            chunk.event_seq,
            source_mode,
            chunk.source_text_sha256,
            chunk.source_char_len,
            chunk.chunk_index,
            chunk.chunk_count,
            chunk.chunk_start_char,
            chunk.chunk_end_char,
            len(chunk.text),
            sha256_text(chunk.text),
            chunk_target_chars,
            chunk_overlap_chars,
            dimensions,
            vector_blob(embedding),
            now_ms,
        )
        for chunk, embedding in zip(chunks, embeddings)
    ]


def write_event_records(sidecar, records):
    sidecar.executemany(
        """
        INSERT INTO event_embeddings
            (event_id, model_key, source_mode, event_seq, text_sha256, dimensions, embedding_f32, embedded_at_ms)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(event_id, model_key) DO UPDATE SET
            source_mode = excluded.source_mode,
            event_seq = excluded.event_seq,
            text_sha256 = excluded.text_sha256,
            dimensions = excluded.dimensions,
            embedding_f32 = excluded.embedding_f32,
            embedded_at_ms = excluded.embedded_at_ms
        """,
        records,
    )


def write_chunk_records(
    sidecar,
    docs: list[SourceDocument],
    records,
    model_key,
    source_mode,
    chunk_target_chars,
    chunk_overlap_chars,
):
    delete_records = [
        (doc.event_id, model_key, source_mode, chunk_target_chars, chunk_overlap_chars)
        for doc in docs
    ]
    sidecar.executemany(
        """
        DELETE FROM event_chunk_embeddings
        WHERE event_id = ?
          AND model_key = ?
          AND source_mode = ?
          AND chunk_target_chars = ?
          AND chunk_overlap_chars = ?
        """,
        delete_records,
    )
    sidecar.executemany(
        """
        INSERT INTO event_chunk_embeddings (
            event_id,
            model_key,
            event_seq,
            source_mode,
            source_text_sha256,
            source_char_len,
            chunk_index,
            chunk_count,
            chunk_start_char,
            chunk_end_char,
            chunk_char_len,
            chunk_text_sha256,
            chunk_target_chars,
            chunk_overlap_chars,
            dimensions,
            embedding_f32,
            embedded_at_ms
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        records,
    )


def load_text_embedding():
    try:
        from fastembed import TextEmbedding
    except ModuleNotFoundError as error:
        raise SystemExit(
            "missing Python dependency 'fastembed'; install it in this environment to run embeddings"
        ) from error
    return TextEmbedding


def validate_args(parser, args):
    if args.fetch_batch <= 0:
        parser.error("--fetch-batch must be greater than 0")
    if args.embed_batch <= 0:
        parser.error("--embed-batch must be greater than 0")
    if args.threads <= 0:
        parser.error("--threads must be greater than 0")
    if args.limit is not None and args.limit < 0:
        parser.error("--limit must be greater than or equal to 0")
    if args.dimensions <= 0:
        parser.error("--dimensions must be greater than 0")
    if args.embedding_mode == EMBEDDING_MODE_CHUNKED:
        if args.chunk_target_chars <= 0:
            parser.error("--chunk-target-chars must be greater than 0")
        if args.chunk_overlap_chars < 0:
            parser.error("--chunk-overlap-chars must be at least 0")
        if args.chunk_overlap_chars >= args.chunk_target_chars:
            parser.error("--chunk-overlap-chars must be less than --chunk-target-chars")
    work_path = pathlib.Path(args.work_db).expanduser().resolve()
    sidecar_path = pathlib.Path(args.sidecar).expanduser().resolve()
    if work_path == sidecar_path:
        parser.error("--sidecar must be separate from --work-db")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--work-db", required=True)
    parser.add_argument("--sidecar", required=True)
    parser.add_argument("--metrics", required=True)
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument(
        "--model-key",
        default=DEFAULT_MODEL_KEY,
    )
    parser.add_argument("--dimensions", type=int, default=DEFAULT_DIMENSIONS)
    parser.add_argument("--cache-dir", default="/tmp/fastembed_cache")
    parser.add_argument("--fetch-batch", type=int, default=2048)
    parser.add_argument("--embed-batch", type=int, default=256)
    parser.add_argument("--threads", type=int, default=os.cpu_count() or 1)
    parser.add_argument("--limit", type=int)
    parser.add_argument(
        "--embedding-mode",
        choices=[EMBEDDING_MODE_EVENT, EMBEDDING_MODE_CHUNKED],
        default=EMBEDDING_MODE_EVENT,
        help="Embed whole event preview rows or deterministic character chunks",
    )
    parser.add_argument(
        "--source-mode",
        choices=[SOURCE_MODE_SEMANTIC_PAYLOAD, SOURCE_MODE_EVENT_SEARCH_PREVIEW],
        default=SOURCE_MODE_SEMANTIC_PAYLOAD,
        help="Text source to embed; semantic_payload matches the product path",
    )
    parser.add_argument(
        "--chunk-target-chars",
        type=int,
        default=1200,
        help="Target/max characters per chunk in --embedding-mode chunked",
    )
    parser.add_argument(
        "--chunk-overlap-chars",
        type=int,
        default=200,
        help="Characters repeated from the previous chunk in --embedding-mode chunked",
    )
    parser.add_argument(
        "--resume-after-sidecar-max-seq",
        action="store_true",
        help="Only process rows with event_seq greater than the sidecar max for this model/config",
    )
    parser.add_argument(
        "--missing-from-sidecar",
        action="store_true",
        help="Only process work-db events missing from the sidecar for this model/config",
    )
    args = parser.parse_args()
    validate_args(parser, args)

    started = time.perf_counter()
    model_started = time.perf_counter()
    TextEmbedding = load_text_embedding()
    model = TextEmbedding(
        model_name=args.model,
        cache_dir=args.cache_dir,
        threads=args.threads,
    )
    model_seconds = time.perf_counter() - model_started

    source = sqlite3.connect(f"file:{args.work_db}?mode=ro", uri=True)
    source.execute("PRAGMA query_only=ON")
    sidecar = setup_sidecar(args.sidecar)
    sidecar.execute(
        "INSERT OR IGNORE INTO embedding_models VALUES (?, ?, ?, ?)",
        (
            args.model_key,
            args.model,
            args.dimensions,
            int(time.time() * 1000),
        ),
    )
    sidecar.commit()

    events = 0
    chunks = 0
    embed_seconds = 0.0
    write_seconds = 0.0
    fetch_seconds = 0.0
    last_seq = None
    if args.resume_after_sidecar_max_seq:
        if args.embedding_mode == EMBEDDING_MODE_CHUNKED:
            row = sidecar.execute(
                """
                SELECT MAX(event_seq)
                FROM event_chunk_embeddings
                WHERE model_key = ?
                  AND source_mode = ?
                  AND chunk_target_chars = ?
                  AND chunk_overlap_chars = ?
                """,
                (
                    args.model_key,
                    args.source_mode,
                    args.chunk_target_chars,
                    args.chunk_overlap_chars,
                ),
            ).fetchone()
        else:
            row = sidecar.execute(
                """
                SELECT MAX(event_seq)
                FROM event_embeddings
                WHERE model_key = ?
                  AND source_mode = ?
                """,
                (args.model_key, args.source_mode),
            ).fetchone()
        last_seq = row[0] if row and row[0] is not None else None

    missing_docs = None
    if args.missing_from_sidecar:
        fetch_started = time.perf_counter()
        missing_docs = fetch_missing_from_sidecar(
            source,
            args.sidecar,
            args.model_key,
            args.limit,
            args.embedding_mode,
            args.source_mode,
            args.chunk_target_chars,
            args.chunk_overlap_chars,
        )
        fetch_seconds += time.perf_counter() - fetch_started

    missing_offset = 0
    progress_every = max(args.fetch_batch * 10, 1)
    next_progress = progress_every
    while True:
        if args.limit is not None and events >= args.limit:
            break
        if missing_docs is not None:
            batch = missing_docs[missing_offset : missing_offset + args.fetch_batch]
            missing_offset += len(batch)
        else:
            remaining = None if args.limit is None else args.limit - events
            fetch_limit = args.fetch_batch if remaining is None else min(args.fetch_batch, remaining)
            fetch_started = time.perf_counter()
            batch = fetch_batch(source, args.source_mode, last_seq, fetch_limit)
            fetch_seconds += time.perf_counter() - fetch_started
        if not batch:
            break
        last_seq = batch[-1].event_seq

        if args.embedding_mode == EMBEDDING_MODE_CHUNKED:
            text_units = [
                chunk
                for doc in batch
                for chunk in chunk_document(
                    doc,
                    args.chunk_target_chars,
                    args.chunk_overlap_chars,
                )
            ]
            texts = [semantic_passage_text(chunk.text) for chunk in text_units]
        else:
            text_units = batch
            texts = [semantic_passage_text(doc.text) for doc in batch]

        embed_started = time.perf_counter()
        embeddings = list(model.embed(texts, batch_size=args.embed_batch))
        embed_seconds += time.perf_counter() - embed_started

        now_ms = int(time.time() * 1000)
        if args.embedding_mode == EMBEDDING_MODE_CHUNKED:
            records = chunk_records(
                text_units,
                embeddings,
                args.model_key,
                args.source_mode,
                args.chunk_target_chars,
                args.chunk_overlap_chars,
                args.dimensions,
                now_ms,
            )
        else:
            records = event_records(
                text_units,
                embeddings,
                args.model_key,
                args.source_mode,
                args.dimensions,
                now_ms,
            )

        write_started = time.perf_counter()
        if args.embedding_mode == EMBEDDING_MODE_CHUNKED:
            write_chunk_records(
                sidecar,
                batch,
                records,
                args.model_key,
                args.source_mode,
                args.chunk_target_chars,
                args.chunk_overlap_chars,
            )
        else:
            write_event_records(sidecar, records)
        sidecar.commit()
        write_seconds += time.perf_counter() - write_started
        events += len(batch)
        chunks += len(records)
        if events >= next_progress:
            elapsed = time.perf_counter() - started
            print(
                json.dumps(
                    {
                        "events": events,
                        "chunks": chunks,
                        "elapsed_seconds": elapsed,
                        "events_per_second": events / elapsed if elapsed else 0,
                        "chunks_per_second": chunks / elapsed if elapsed else 0,
                    }
                ),
                flush=True,
            )
            while events >= next_progress:
                next_progress += progress_every

    try:
        sidecar.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    except sqlite3.OperationalError:
        pass
    sidecar.commit()
    sidecar.close()
    source.close()

    total_seconds = time.perf_counter() - started
    chunk_multiplier = chunks / events if events else 0
    metrics = {
        "rows": chunks,
        "events": events,
        "chunks": chunks,
        "chunk_multiplier": chunk_multiplier,
        "total_seconds": total_seconds,
        "model_seconds": model_seconds,
        "fetch_seconds": fetch_seconds,
        "embed_seconds": embed_seconds,
        "write_seconds": write_seconds,
        "rows_per_second": chunks / total_seconds if total_seconds else 0,
        "events_per_second": events / total_seconds if total_seconds else 0,
        "chunks_per_second": chunks / total_seconds if total_seconds else 0,
        "model": args.model,
        "model_key": args.model_key,
        "dimensions": args.dimensions,
        "threads": args.threads,
        "fetch_batch": args.fetch_batch,
        "embed_batch": args.embed_batch,
        "embedding_mode": args.embedding_mode,
        "source_mode": args.source_mode,
        "chunk_target_chars": args.chunk_target_chars,
        "chunk_overlap_chars": args.chunk_overlap_chars,
        "resume_after_sidecar_max_seq": args.resume_after_sidecar_max_seq,
        "missing_from_sidecar": args.missing_from_sidecar,
        "sidecar_bytes": sidecar_file_bytes(args.sidecar),
    }
    with open(args.metrics, "w", encoding="utf-8") as handle:
        json.dump(metrics, handle, indent=2)
    print(json.dumps(metrics, indent=2))


if __name__ == "__main__":
    main()
