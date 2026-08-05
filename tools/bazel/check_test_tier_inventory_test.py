#!/usr/bin/env python3

import unittest

from check_test_tier_inventory import InventoryError, validate_inventory


class TestTierInventoryTest(unittest.TestCase):
    def test_exact_routing_and_manual_exclusion_pass(self) -> None:
        self.assertEqual(
            validate_inventory(
                {"//:routed", "//:fixture"},
                {"//:routed"},
                {"//:fixture"},
            ),
            (1, 1),
        )

    def test_new_ordinary_test_fails_closed(self) -> None:
        with self.assertRaisesRegex(InventoryError, "//:new_test"):
            validate_inventory(
                {"//:routed", "//:new_test"},
                {"//:routed"},
                set(),
            )

    def test_manual_test_must_not_be_routed(self) -> None:
        with self.assertRaisesRegex(InventoryError, "manual tests"):
            validate_inventory(
                {"//:routed"},
                {"//:routed"},
                {"//:routed"},
            )

    def test_release_aggregate_must_expand_only_to_tests(self) -> None:
        with self.assertRaisesRegex(InventoryError, "non-test labels"):
            validate_inventory(
                {"//:routed"},
                {"//:routed", "//:not-a-test"},
                set(),
            )


if __name__ == "__main__":
    unittest.main()
