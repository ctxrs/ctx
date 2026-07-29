#!/usr/bin/env python3

import json
import pathlib
import sys
import tempfile
import unittest


SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import semantic_worker_bench as worker_bench


class SemanticWorkerBenchTest(unittest.TestCase):
    def test_semantic_index_bytes_counts_generation_tree(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = pathlib.Path(temp_dir) / "search" / "semantic"
            (root / "generation-a").mkdir(parents=True)
            (root / "control.json").write_bytes(b"abc")
            (root / "generation-a" / "vectors.f32").write_bytes(b"defghi")

            self.assertEqual(worker_bench.semantic_index_bytes(str(root)), 9)

    def test_extract_semantic_index_path_prefers_status_and_falls_back_to_data_root(self):
        self.assertEqual(
            worker_bench.extract_semantic_index_path(
                {"semantic": {"flat_f32": {"path": "/tmp/ctx/search/semantic"}}},
                data_root="/tmp/other",
            ),
            "/tmp/ctx/search/semantic",
        )
        self.assertEqual(
            worker_bench.extract_semantic_index_path(data_root="/tmp/ctx-root"),
            "/tmp/ctx-root/search/semantic",
        )

    def test_sanitize_search_json_drops_results_and_keeps_worker_coverage(self):
        data = {
            "schema_version": 1,
            "payload_type": "search_results",
            "results": [{"snippet": "private one"}, {"snippet": "private two"}],
            "freshness": {"mode": "background", "status": "completed"},
            "retrieval": {
                "requested_mode": "hybrid",
                "effective_mode": "hybrid",
                "semantic_status": "partial",
                "semantic_weight": 0.35,
                "vector_path": "/tmp/ctx/search/semantic",
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
                    "last_error": "failed reading /home/private/search/semantic",
                    "coverage": {
                        "queued_items_estimate": 6,
                        "coverage_ratio": 0.5,
                        "path": "/home/private/relational.sqlite",
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
        self.assertNotIn("lock_path", sanitized["retrieval"]["worker"])
        self.assertEqual(sanitized["retrieval"]["diagnostics"]["vector_scan_ms"], 7)
        self.assertEqual(sanitized["retrieval"]["diagnostics"]["chunks_scanned"], 11)
        self.assertEqual(sanitized["retrieval"]["diagnostics"]["semantic_candidates"], 13)
        self.assertNotIn("private_path", sanitized["retrieval"]["diagnostics"])

    def test_redact_argv_hides_search_query_and_paths(self):
        redacted = worker_bench.redact_argv(
            [
                "/home/private/bin/ctx",
                "--data-root",
                "/tmp/ctx",
                "search",
                "private query text",
                "--term",
                "private term",
                "--format=json",
            ]
        )
        serialized = json.dumps(redacted)

        self.assertIn("path:sha256", serialized)
        self.assertIn("query:sha256", serialized)
        self.assertIn("value:sha256", serialized)
        self.assertNotIn("/home/private/bin/ctx", serialized)
        self.assertNotIn("/tmp/ctx", serialized)
        self.assertNotIn("private query text", serialized)
        self.assertNotIn("private term", serialized)

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
            ({"id": "123e4567-e89b-12d3-a456-426614174000"}, {}, "raw UUID"),
            ({"value": "/home/private/.ctx"}, {}, "local path"),
            (
                {"value": "private query text"},
                {"raw_queries": ["private query text"]},
                "raw query text",
            ),
            (
                {"value": "/tmp/private-ctx"},
                {"raw_paths": ["/tmp/private-ctx"]},
                "raw local path",
            ),
        ]
        for payload, kwargs, message in cases:
            with self.subTest(message=message):
                with self.assertRaisesRegex(SystemExit, message):
                    worker_bench.validate_private_output(payload, **kwargs)

        for key in ("cursor", "last_error", "path", "snippet", "source_path", "stderr"):
            with self.subTest(key=key):
                with self.assertRaisesRegex(SystemExit, "raw result keys"):
                    worker_bench.validate_private_output({key: "private"})


if __name__ == "__main__":
    unittest.main()
