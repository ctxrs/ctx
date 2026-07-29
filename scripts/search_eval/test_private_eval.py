#!/usr/bin/env python3

import io
import json
import os
import pathlib
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import private_eval


class PrivateEvalManifestScaffoldTest(unittest.TestCase):
    def test_search_json_hashes_emit_only_salted_hashes(self):
        payload = {
            "results": [
                {
                    "ctx_event_id": "event-1",
                    "ctx_session_id": "session-1",
                    "snippet": "private text from /home/private/session.jsonl",
                },
                {
                    "ctx_event_id": "event-2",
                    "ctx_session_id": "session-1",
                    "citations": [{"provider_session_id": "provider-private"}],
                },
                {
                    "ctx_event_id": "event-3",
                    "ctx_session_id": "session-2",
                },
            ]
        }

        scaffold = private_eval.search_json_hashes("salt", payload, limit=2)
        private_eval.validate_manifest_scaffold(scaffold)
        serialized = json.dumps(scaffold)

        self.assertEqual(
            scaffold,
            {
                "expected": {
                    "event_hashes": [
                        private_eval.stable_hash("salt", "event", "event-1"),
                        private_eval.stable_hash("salt", "event", "event-2"),
                    ],
                    "session_hashes": [
                        private_eval.stable_hash("salt", "session", "session-1")
                    ],
                }
            },
        )
        self.assertNotIn("event-1", serialized)
        self.assertNotIn("session-1", serialized)
        self.assertNotIn("private text", serialized)
        self.assertNotIn("/home/private", serialized)
        self.assertNotIn("provider-private", serialized)

    def test_manifest_scaffold_validation_rejects_non_hash_output(self):
        with self.assertRaisesRegex(SystemExit, "only contain expected"):
            private_eval.validate_manifest_scaffold(
                {
                    "query": "private query",
                    "expected": {"event_hashes": [], "session_hashes": []},
                }
            )

        with self.assertRaisesRegex(SystemExit, "non-hash"):
            private_eval.validate_manifest_scaffold(
                {
                    "expected": {
                        "event_hashes": ["event-1"],
                        "session_hashes": [],
                    }
                }
            )

    def test_cli_scaffold_from_search_json(self):
        payload = {
            "results": [
                {"ctx_event_id": "event-1", "ctx_session_id": "session-1"},
                {"ctx_event_id": "event-2", "ctx_session_id": "session-2"},
            ]
        }
        with tempfile.TemporaryDirectory() as temp_dir:
            input_path = pathlib.Path(temp_dir) / "search.json"
            input_path.write_text(json.dumps(payload), encoding="utf-8")
            argv = [
                "private_eval.py",
                "--scaffold-from-search-json",
                str(input_path),
                "--scaffold-limit",
                "1",
            ]
            with mock.patch.dict(os.environ, {"CTX_EVAL_SALT": "salt"}):
                with mock.patch.object(sys, "argv", argv):
                    with mock.patch("sys.stdout", new=io.StringIO()) as stdout:
                        private_eval.main()

        scaffold = json.loads(stdout.getvalue())
        self.assertEqual(
            scaffold,
            {
                "expected": {
                    "event_hashes": [
                        private_eval.stable_hash("salt", "event", "event-1")
                    ],
                    "session_hashes": [
                        private_eval.stable_hash("salt", "session", "session-1")
                    ],
                }
            },
        )


class PrivateEvalRetrievalSummaryTest(unittest.TestCase):
    def test_retrieval_mode_summary_counts_fallbacks_and_effective_modes(self):
        summary = private_eval.retrieval_mode_summary(
            [
                {
                    "requested_mode": "hybrid",
                    "effective_mode": "lexical",
                    "semantic_fallback": True,
                    "diagnostics": {
                        "semantic_candidates": 4,
                        "vector_scan_ms": 20,
                        "chunks_scanned": 100,
                        "private_path": "/home/private/vector",
                    },
                },
                {
                    "requested_mode": "semantic",
                    "effective_mode": "lexical",
                },
                {
                    "requested_mode": "hybrid",
                    "effective_mode": "hybrid",
                    "semantic_fallback": False,
                },
                None,
            ]
        )

        self.assertEqual(summary["retrieval_samples"], 3)
        self.assertEqual(summary["semantic_fallback_count"], 2)
        self.assertAlmostEqual(summary["semantic_fallback_rate"], 2 / 3)
        self.assertEqual(summary["effective_mode_counts"], {"lexical": 2, "hybrid": 1})
        self.assertAlmostEqual(summary["effective_mode_rates"]["lexical"], 2 / 3)
        self.assertAlmostEqual(summary["effective_mode_rates"]["hybrid"], 1 / 3)
        self.assertEqual(summary["diagnostics"]["samples"], 1)
        self.assertEqual(summary["diagnostics"]["semantic_candidates_p95"], 4)
        self.assertEqual(summary["diagnostics"]["vector_scan_ms_p95"], 20)
        self.assertEqual(summary["diagnostics"]["chunks_scanned_max"], 100)
        self.assertNotIn("private_path_p95", summary["diagnostics"])

    def test_retrieval_summary_preserves_safe_diagnostics(self):
        summary = private_eval.retrieval_summary(
            {
                "retrieval": {
                    "requested_mode": "hybrid",
                    "effective_mode": "hybrid",
                    "diagnostics": {
                        "semantic_candidates": 5,
                        "query_embed_ms": 3,
                        "vector_scan_ms": 9,
                        "chunks_scanned": 25,
                        "vector_bytes_read": 4096,
                        "private_path": "/home/private/vector",
                    },
                }
            }
        )

        self.assertEqual(summary["diagnostics"]["query_embed_ms"], 3)
        self.assertEqual(summary["diagnostics"]["semantic_candidates"], 5)
        self.assertEqual(summary["diagnostics"]["vector_scan_ms"], 9)
        self.assertEqual(summary["diagnostics"]["vector_bytes_read"], 4096)
        self.assertNotIn("private_path", summary["diagnostics"])

    def test_backend_comparison_reports_quality_and_latency_deltas(self):
        comparison = private_eval.backend_comparison(
            {
                "lexical": {
                    "hit1": 0.25,
                    "hit5": 0.5,
                    "mrr": 0.4,
                    "p95_ms": 100,
                    "semantic_fallback_rate": 0,
                },
                "hybrid": {
                    "hit1": 0.5,
                    "hit5": 0.75,
                    "mrr": 0.55,
                    "p95_ms": 250,
                    "semantic_fallback_rate": 0.2,
                },
            },
            "lexical",
        )

        self.assertEqual(comparison["hybrid"]["baseline"], "lexical")
        self.assertAlmostEqual(comparison["hybrid"]["hit1_delta"], 0.25)
        self.assertAlmostEqual(comparison["hybrid"]["hit5_delta"], 0.25)
        self.assertAlmostEqual(comparison["hybrid"]["mrr_delta"], 0.15)
        self.assertAlmostEqual(comparison["hybrid"]["p95_ms_delta"], 150)
        self.assertAlmostEqual(comparison["hybrid"]["p95_ratio"], 2.5)
        self.assertAlmostEqual(
            comparison["hybrid"]["semantic_fallback_rate_delta"],
            0.2,
        )


if __name__ == "__main__":
    unittest.main()
