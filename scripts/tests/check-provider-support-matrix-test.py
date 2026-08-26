#!/usr/bin/env python3

from __future__ import annotations

import copy
import importlib.util
import json
from pathlib import Path
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "check-provider-support-matrix.py"
SPEC = importlib.util.spec_from_file_location("provider_support_matrix", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load provider support matrix validator")
matrix = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(matrix)


class ProviderSupportMatrixTest(unittest.TestCase):
    def providers(self) -> list[dict[str, object]]:
        value = json.loads(matrix.MATRIX_PATH.read_text(encoding="utf-8"))
        return value["providers"]

    def test_repository_lineage_claims_are_exact(self) -> None:
        providers = self.providers()
        matrix.validate_provider_lineage_claims(providers)
        strengthened = {
            provider["id"]
            for provider in providers
            if provider["lineage_support"] != {
                "session_relationship": "unknown",
                "event_origin": "unknown",
            }
        }
        self.assertEqual(strengthened, set(matrix.EXPECTED_PROVIDER_LINEAGE_SUPPORT))

    def test_repository_matrix_passes_the_full_checker(self) -> None:
        self.assertEqual(matrix.main(), 0)

    def test_repository_matrix_has_exact_non_vacuous_supported_count(self) -> None:
        providers = self.providers()
        self.assertEqual(len(providers), matrix.EXPECTED_SUPPORTED_PROVIDER_COUNT)
        self.assertTrue(providers)
        self.assertEqual({provider["status"] for provider in providers}, {"supported"})
        self.assertNotIn("windsurf", {provider["id"] for provider in providers})

    def test_configured_root_capability_is_required_for_every_provider(self) -> None:
        providers = self.providers()
        for provider in providers:
            matrix.validate_configured_root(provider["configured_root"], str(provider["id"]))
        states = {provider["configured_root"]["state"] for provider in providers}
        self.assertEqual(states, matrix.ALLOWED_CONFIGURED_ROOT_STATES)

        missing = copy.deepcopy(providers[0])
        missing.pop("configured_root")
        with self.assertRaisesRegex(matrix.MatrixError, r"configured_root must be dict"):
            matrix.validate_provider(missing, 0, set())

    def test_configured_root_capability_rejects_incoherent_shapes(self) -> None:
        with self.assertRaisesRegex(matrix.MatrixError, r"intentional state.*exactly state"):
            matrix.validate_configured_root(
                {
                    "state": "intentional_automatic_exact",
                    "expected_path_kind": "directory",
                },
                "deferred",
            )
        with self.assertRaisesRegex(matrix.MatrixError, r"expander.kind has unsupported value"):
            matrix.validate_configured_root(
                {
                    "state": "enabled",
                    "expected_path_kind": "directory",
                    "expander": {"kind": "guessed_home"},
                },
                "enabled",
            )
        with self.assertRaisesRegex(matrix.MatrixError, r"exact OpenHands root kinds"):
            matrix.validate_configured_root(
                {
                    "state": "enabled",
                    "expected_path_kind": "directory",
                    "expander": {
                        "kind": "openhands_kind_v1",
                        "root_kinds": ["current-conversations"],
                    },
                },
                "openhands",
            )

    def test_codex_copy_claim_stays_deferred(self) -> None:
        providers = copy.deepcopy(self.providers())
        codex = next(provider for provider in providers if provider["id"] == "codex")
        codex["lineage_support"]["event_origin"] = "exact_copy"

        with self.assertRaisesRegex(
            matrix.MatrixError,
            r"providers\[codex\].*exact_relationship/unknown",
        ):
            matrix.validate_provider_lineage_claims(providers)

    def test_intentional_unknown_provider_cannot_be_promoted(self) -> None:
        providers = copy.deepcopy(self.providers())
        deferred = next(
            provider for provider in providers if provider["id"] == "deepagents"
        )
        deferred["lineage_support"] = {
            "session_relationship": "exact_relationship",
            "event_origin": "explicit_no_copy",
        }

        with self.assertRaisesRegex(
            matrix.MatrixError,
            r"providers\[deepagents\].*unknown/unknown",
        ):
            matrix.validate_provider_lineage_claims(providers)


if __name__ == "__main__":
    unittest.main()
