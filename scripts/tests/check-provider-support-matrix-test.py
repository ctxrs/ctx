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
