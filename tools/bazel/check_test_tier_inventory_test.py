#!/usr/bin/env python3

import unittest
from typing import Any

from check_test_tier_inventory import InventoryError, validate_inventory


def inventory(*labels: str) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "aggregate": "//:release",
        "intentional_exclusions": [
            {
                "label": label,
                "classification": "test-fixture",
                "reason": "Synthetic fixture for the inventory checker.",
            }
            for label in sorted(labels)
        ],
    }


class TestTierInventoryTest(unittest.TestCase):
    def test_exact_routing_and_manual_exclusion_pass(self) -> None:
        self.assertEqual(
            validate_inventory(
                {"//:routed", "//:fixture"},
                {"//:routed"},
                {"//:fixture"},
                inventory("//:fixture"),
            ),
            (1, 1),
        )

    def test_new_ordinary_test_fails_closed(self) -> None:
        with self.assertRaisesRegex(InventoryError, "//:new_test"):
            validate_inventory(
                {"//:routed", "//:new_test"},
                {"//:routed"},
                set(),
                inventory(),
            )

    def test_exclusion_without_manual_tag_fails_closed(self) -> None:
        with self.assertRaisesRegex(InventoryError, "manual tag"):
            validate_inventory(
                {"//:routed", "//:fixture"},
                {"//:routed"},
                set(),
                inventory("//:fixture"),
            )

    def test_routed_exclusion_must_be_removed(self) -> None:
        with self.assertRaisesRegex(InventoryError, "stale or routed"):
            validate_inventory(
                {"//:routed"},
                {"//:routed"},
                {"//:routed"},
                inventory("//:routed"),
            )

    def test_exclusions_require_a_reason(self) -> None:
        document = inventory("//:fixture")
        document["intentional_exclusions"][0]["reason"] = ""
        with self.assertRaisesRegex(InventoryError, "lacks.*reason"):
            validate_inventory(
                {"//:fixture"},
                set(),
                {"//:fixture"},
                document,
            )


if __name__ == "__main__":
    unittest.main()
