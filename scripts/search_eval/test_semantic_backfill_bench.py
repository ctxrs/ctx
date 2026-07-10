#!/usr/bin/env python3

import io
import json
import pathlib
import sqlite3
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import semantic_backfill_bench as bench
import semantic_worker_bench as worker_bench


class SemanticBackfillBenchTest(unittest.TestCase):
    def test_e5_defaults_and_role_prefixes_match_production(self):
        self.assertEqual(bench.DEFAULT_MODEL, "intfloat/multilingual-e5-small")
        self.assertEqual(
            bench.DEFAULT_MODEL_KEY,
            "fastembed:intfloat-multilingual-e5-small:"
            "e5-query-passage:semantic-lite-turn-1200-200-v3",
        )
        self.assertEqual(bench.DEFAULT_DIMENSIONS, 384)
        self.assertEqual(bench.semantic_query_text("find it"), "query: find it")
        self.assertEqual(
            bench.semantic_query_text("  query: find it"), "query: find it"
        )
        self.assertEqual(
            bench.semantic_passage_text("found it"), "passage: found it"
        )
        self.assertEqual(
            bench.semantic_passage_text("  passage: found it"), "passage: found it"
        )

    def test_chunk_document_uses_deterministic_char_ranges(self):
        doc = bench.SourceDocument(
            event_id="event-1",
            event_seq=42,
            text="abcdefghijklmnopqrstuvwxyz",
        )

        chunks = bench.chunk_document(doc, target_chars=10, overlap_chars=3)

        self.assertEqual(
            [(chunk.chunk_start_char, chunk.chunk_end_char, chunk.text) for chunk in chunks],
            [
                (0, 10, "abcdefghij"),
                (7, 17, "hijklmnopq"),
                (14, 24, "opqrstuvwx"),
                (21, 26, "vwxyz"),
            ],
        )
        self.assertEqual([chunk.chunk_index for chunk in chunks], [0, 1, 2, 3])
        self.assertEqual({chunk.chunk_count for chunk in chunks}, {4})
        self.assertEqual({chunk.source_char_len for chunk in chunks}, {26})

    def test_sidecar_schema_includes_chunk_metadata(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            sidecar_path = pathlib.Path(temp_dir) / "semantic.sqlite"
            conn = bench.setup_sidecar(str(sidecar_path))
            try:
                columns = {
                    row[1]
                    for row in conn.execute(
                        "PRAGMA table_info(event_chunk_embeddings)"
                    ).fetchall()
                }
            finally:
                conn.close()

        self.assertTrue(
            {
                "source_mode",
                "source_text_sha256",
                "source_char_len",
                "chunk_index",
                "chunk_count",
                "chunk_start_char",
                "chunk_end_char",
                "chunk_char_len",
                "chunk_text_sha256",
                "chunk_target_chars",
                "chunk_overlap_chars",
            }.issubset(columns)
        )

    def test_main_writes_event_and_chunk_metrics_with_fake_model(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = pathlib.Path(temp_dir)
            work_db = temp_path / "work.sqlite"
            event_sidecar = temp_path / "event-sidecar.sqlite"
            event_metrics = temp_path / "event-metrics.json"
            chunk_sidecar = temp_path / "chunk-sidecar.sqlite"
            chunk_metrics = temp_path / "chunk-metrics.json"
            self.create_work_db(work_db)

            event_embedding_texts = self.run_main(
                [
                    "--work-db",
                    str(work_db),
                    "--sidecar",
                    str(event_sidecar),
                    "--metrics",
                    str(event_metrics),
                    "--dimensions",
                    "4",
                    "--limit",
                    "2",
                    "--fetch-batch",
                    "1",
                    "--source-mode",
                    "event_search_preview",
                ]
            )
            event_data = json.loads(event_metrics.read_text(encoding="utf-8"))
            self.assertEqual(event_data["embedding_mode"], "event")
            self.assertEqual(event_data["model"], "intfloat/multilingual-e5-small")
            self.assertEqual(
                event_data["model_key"],
                "fastembed:intfloat-multilingual-e5-small:"
                "e5-query-passage:semantic-lite-turn-1200-200-v3",
            )
            self.assertEqual(
                event_embedding_texts,
                ["passage: abcdefghijklmnop", "passage: 12345"],
            )
            self.assertEqual(event_data["source_mode"], "event_search_preview")
            self.assertEqual(event_data["events"], 2)
            self.assertEqual(event_data["chunks"], 2)
            self.assertEqual(event_data["chunk_multiplier"], 1)
            with sqlite3.connect(event_sidecar) as conn:
                event_rows = conn.execute(
                    """
                    SELECT source_mode, COUNT(*)
                    FROM event_embeddings
                    GROUP BY source_mode
                    """
                ).fetchall()
            self.assertEqual(event_rows, [("event_search_preview", 2)])

            chunk_embedding_texts = self.run_main(
                [
                    "--work-db",
                    str(work_db),
                    "--sidecar",
                    str(chunk_sidecar),
                    "--metrics",
                    str(chunk_metrics),
                    "--dimensions",
                    "4",
                    "--limit",
                    "2",
                    "--embedding-mode",
                    "chunked",
                    "--source-mode",
                    "event_search_preview",
                    "--chunk-target-chars",
                    "10",
                    "--chunk-overlap-chars",
                    "2",
                ]
            )
            chunk_data = json.loads(chunk_metrics.read_text(encoding="utf-8"))
            self.assertEqual(chunk_data["embedding_mode"], "chunked")
            self.assertEqual(chunk_data["source_mode"], "event_search_preview")
            self.assertEqual(chunk_data["events"], 2)
            self.assertEqual(chunk_data["chunks"], 3)
            self.assertEqual(chunk_data["chunk_multiplier"], 1.5)
            self.assertEqual(
                chunk_embedding_texts,
                [
                    "passage: abcdefghij",
                    "passage: ijklmnop",
                    "passage: 12345",
                ],
            )
            with sqlite3.connect(chunk_sidecar) as conn:
                rows = conn.execute(
                    """
                    SELECT event_id, chunk_index, chunk_count, chunk_start_char, chunk_end_char
                    FROM event_chunk_embeddings
                    ORDER BY event_seq, chunk_index
                    """
                ).fetchall()
            self.assertEqual(
                rows,
                [
                    ("event-1", 0, 2, 0, 10),
                    ("event-1", 1, 2, 8, 16),
                    ("event-2", 0, 1, 0, 5),
                ],
            )

    def test_semantic_payload_source_reads_beyond_preview_and_filters_hidden_rows(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            work_db = pathlib.Path(temp_dir) / "work.sqlite"
            tail = "semantic-tail-needle"
            long_text = f"visible {'x' * 2200} {tail}"
            with sqlite3.connect(work_db) as conn:
                conn.execute(
                    """
                    CREATE TABLE events (
                        id TEXT PRIMARY KEY,
                        seq INTEGER NOT NULL,
                        payload_json TEXT NOT NULL,
                        visibility TEXT NOT NULL,
                        redaction_state TEXT NOT NULL,
                        sync_state TEXT NOT NULL,
                        deleted_at_ms INTEGER
                    )
                    """
                )
                conn.execute(
                    """
                    CREATE TABLE event_search (
                        event_id TEXT PRIMARY KEY,
                        safe_preview_text TEXT NOT NULL
                    )
                    """
                )
                conn.executemany(
                    """
                    INSERT INTO events
                    (id, seq, payload_json, visibility, redaction_state, sync_state, deleted_at_ms)
                    VALUES (?, ?, ?, ?, ?, ?, NULL)
                    """,
                    [
                        (
                            "event-1",
                            1,
                            json.dumps({"text": long_text}),
                            "local_only",
                            "safe_preview",
                            "local_only",
                        ),
                        (
                            "event-2",
                            2,
                            json.dumps({"text": f"hidden {tail}"}),
                            "withheld",
                            "safe_preview",
                            "local_only",
                        ),
                    ],
                )
                conn.executemany(
                    "INSERT INTO event_search (event_id, safe_preview_text) VALUES (?, ?)",
                    [
                        ("event-1", long_text[:2048]),
                        ("event-2", f"hidden {tail}"),
                    ],
                )

                docs = bench.fetch_batch(
                    conn,
                    bench.SOURCE_MODE_SEMANTIC_PAYLOAD,
                    last_seq=None,
                    limit=10,
                )

            self.assertEqual([doc.event_id for doc in docs], ["event-1"])
            self.assertIn(tail, docs[0].text)

    @staticmethod
    def create_work_db(path: pathlib.Path):
        with sqlite3.connect(path) as conn:
            conn.execute(
                """
                CREATE TABLE events (
                    id TEXT PRIMARY KEY,
                    seq INTEGER NOT NULL,
                    deleted_at_ms INTEGER
                )
                """
            )
            conn.execute(
                """
                CREATE TABLE event_search (
                    event_id TEXT PRIMARY KEY,
                    safe_preview_text TEXT NOT NULL
                )
                """
            )
            conn.executemany(
                "INSERT INTO events (id, seq, deleted_at_ms) VALUES (?, ?, NULL)",
                [("event-1", 1), ("event-2", 2)],
            )
            conn.executemany(
                "INSERT INTO event_search (event_id, safe_preview_text) VALUES (?, ?)",
                [("event-1", "abcdefghijklmnop"), ("event-2", "12345")],
            )

    @staticmethod
    def run_main(argv):
        embedded_texts = []

        class FakeEmbedding:
            def __init__(self, **kwargs):
                self.kwargs = kwargs

            def embed(self, texts, batch_size):
                embedded_texts.extend(texts)
                for text in texts:
                    yield [float(len(text)), float(batch_size), 1.0, 0.0]

        with mock.patch.object(bench, "load_text_embedding", return_value=FakeEmbedding):
            with mock.patch.object(sys, "argv", ["semantic_backfill_bench.py", *argv]):
                with mock.patch("sys.stdout", new=io.StringIO()):
                    bench.main()
        return embedded_texts


class SemanticWorkerBenchTest(unittest.TestCase):
    def test_sidecar_file_bytes_includes_sqlite_companion_files(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            sidecar = pathlib.Path(temp_dir) / "vectors.sqlite"
            sidecar.write_bytes(b"abc")
            pathlib.Path(str(sidecar) + "-wal").write_bytes(b"de")
            pathlib.Path(str(sidecar) + "-shm").write_bytes(b"fghi")

            self.assertEqual(worker_bench.sidecar_file_bytes(str(sidecar)), 9)

    def test_extract_sidecar_path_prefers_retrieval_and_falls_back_to_data_root(self):
        self.assertEqual(
            worker_bench.extract_sidecar_path(
                {"retrieval": {"vector_path": "/tmp/ctx/vectors.sqlite"}},
                data_root="/tmp/other",
            ),
            "/tmp/ctx/vectors.sqlite",
        )
        self.assertEqual(
            worker_bench.extract_sidecar_path(data_root="/tmp/ctx-root"),
            "/tmp/ctx-root/vectors.sqlite",
        )

    def test_sanitize_search_json_drops_results_and_keeps_worker_coverage(self):
        data = {
            "schema_version": 1,
            "payload_type": "search_results",
            "results": [
                {"snippet": "private one"},
                {"snippet": "private two"},
            ],
            "freshness": {"mode": "background", "status": "completed"},
            "retrieval": {
                "requested_mode": "hybrid",
                "effective_mode": "hybrid",
                "semantic_status": "partial",
                "semantic_weight": 0.35,
                "vector_path": "/tmp/ctx/vectors.sqlite",
                "coverage": {
                    "embedded_items": 7,
                    "embedded_chunks": 11,
                    "searchable_items": 13,
                    "indexed_now": 0,
                    "source_path": "/home/private/source.jsonl",
                },
                "worker": {
                    "status": "running",
                    "running": True,
                    "pid": 1234,
                    "last_error": "failed reading /home/private/vectors.sqlite",
                    "coverage": {
                        "queued_items_estimate": 6,
                        "coverage_ratio": 0.5,
                        "path": "/home/private/work.sqlite",
                    },
                    "lock_path": "/tmp/private.lock",
                },
                "diagnostics": {
                    "semantic_candidates": 13,
                    "query_embed_ms": 3,
                    "vector_scan_ms": 7,
                    "chunks_scanned": 11,
                    "vector_bytes_read": 1024,
                    "events_scored": 5,
                    "private_path": "/home/private/vector",
                },
            },
        }

        sanitized = worker_bench.sanitize_search_json(data)

        self.assertEqual(sanitized["result_count"], 2)
        self.assertNotIn("results", sanitized)
        self.assertEqual(sanitized["retrieval"]["requested_mode"], "hybrid")
        self.assertTrue(sanitized["retrieval"]["has_vector_path"])
        self.assertNotIn("vector_path", sanitized["retrieval"])
        self.assertNotIn("source_path", sanitized["retrieval"]["coverage"])
        self.assertEqual(
            sanitized["retrieval"]["worker"]["coverage"]["queued_items_estimate"],
            6,
        )
        self.assertNotIn("last_error", sanitized["retrieval"]["worker"])
        self.assertTrue(sanitized["retrieval"]["worker"]["last_error_present"])
        self.assertGreater(sanitized["retrieval"]["worker"]["last_error_bytes"], 0)
        self.assertNotIn("lock_path", sanitized["retrieval"]["worker"])
        self.assertEqual(sanitized["retrieval"]["diagnostics"]["vector_scan_ms"], 7)
        self.assertEqual(sanitized["retrieval"]["diagnostics"]["chunks_scanned"], 11)
        self.assertEqual(sanitized["retrieval"]["diagnostics"]["semantic_candidates"], 13)
        self.assertNotIn("private_path", sanitized["retrieval"]["diagnostics"])

    def test_redact_argv_hides_search_query(self):
        redacted = worker_bench.redact_argv(
            [
                "cargo",
                "run",
                "-q",
                "-p",
                "ctx",
                "--",
                "--data-root",
                "/tmp/ctx",
                "search",
                "private query text",
                "--term",
                "private term",
                "--json",
            ]
        )
        serialized = json.dumps(redacted)

        self.assertIn("--data-root", redacted)
        self.assertIn("path:sha256", serialized)
        self.assertIn("query:sha256", serialized)
        self.assertIn("value:sha256", serialized)
        self.assertNotIn("/tmp/ctx", serialized)
        self.assertNotIn("private query text", serialized)
        self.assertNotIn("private term", serialized)

    def test_redact_argv_hides_ctx_command_path(self):
        redacted = worker_bench.redact_argv(
            [
                "/home/private/bin/ctx",
                "--data-root=/Users/private/.ctx",
                "status",
                "--json",
            ]
        )
        serialized = json.dumps(redacted)

        self.assertIn("path:sha256", serialized)
        self.assertNotIn("/home/private/bin/ctx", serialized)
        self.assertNotIn("/Users/private/.ctx", serialized)

    def test_command_summary_summarizes_stderr_without_text(self):
        summary = worker_bench.command_summary(
            {
                "argv": ["ctx", "status", "--json"],
                "returncode": 1,
                "wall_ms": 12.5,
                "stderr_summary": worker_bench.private_text_summary(
                    "failed opening /home/private/vectors.sqlite"
                ),
            }
        )
        serialized = json.dumps(summary)

        self.assertTrue(summary["stderr_present"])
        self.assertGreater(summary["stderr_bytes"], 0)
        self.assertIn("stderr_sha256", summary)
        self.assertNotIn("stderr", summary)
        self.assertNotIn("/home/private/vectors.sqlite", serialized)

    def test_ctx_command_summary_does_not_store_raw_command_path(self):
        summary = worker_bench.ctx_command_summary(
            "/home/private/bin/ctx --profile private"
        )
        serialized = json.dumps(summary)

        self.assertEqual(summary["argc"], 3)
        self.assertIn("sha256", summary)
        self.assertIn("path:sha256", serialized)
        self.assertNotIn("/home/private/bin/ctx", serialized)

    def test_validate_private_output_rejects_private_material(self):
        safe_payload = {
            "config": {
                "ctx_command_summary": {
                    "argv": ["<path:sha256:abc:chars:9>"],
                    "sha256": "abc",
                }
            },
            "query_hash": "abc",
        }
        worker_bench.validate_private_output(
            safe_payload,
            raw_queries=["private query text"],
            raw_paths=["/tmp/private-ctx"],
        )

        cases = [
            (
                "raw UUID",
                {"id": "123e4567-e89b-12d3-a456-426614174000"},
                {},
                "raw UUID",
            ),
            ("local home path", {"value": "/home/private/.ctx"}, {}, "local path"),
            (
                "raw query text",
                {"value": "private query text"},
                {"raw_queries": ["private query text"]},
                "raw query text",
            ),
            (
                "raw explicit path",
                {"value": "/tmp/private-ctx"},
                {"raw_paths": ["/tmp/private-ctx"]},
                "raw local path",
            ),
        ]
        for _name, payload, kwargs, message in cases:
            with self.subTest(message=message):
                with self.assertRaisesRegex(SystemExit, message):
                    worker_bench.validate_private_output(payload, **kwargs)

        for key in (
            "cursor",
            "last_error",
            "path",
            "snippet",
            "source_path",
            "stderr",
        ):
            with self.subTest(key=key):
                with self.assertRaisesRegex(SystemExit, "raw result keys"):
                    worker_bench.validate_private_output({key: "private"})


if __name__ == "__main__":
    unittest.main()
