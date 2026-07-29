#!/usr/bin/env python3

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


CHECKER = Path(__file__).resolve().parents[1] / "check-pro-price-consistency.py"
PRICE_ID_STAGING = "price_1TRgNGFkRiblJF0BIwWGnVn5"
PRICE_ID_PRODUCTION = "price_1TxrsLC08fLWVHBUqfW8Degu"
RUNTIME_SURFACES = (
    "crates/ctx-cli/src/cli.rs",
    "crates/ctx-cli/src/commands/status.rs",
    "crates/ctx-cli/src/local_usage/report.rs",
    "crates/ctx-cli/src/pro/commercial_lifecycle.rs",
    "crates/ctx-cli/src/pro/lifecycle_commands.rs",
)
PRICE_DOCS = (
    "docs/cli-reference.md",
    "docs/contracts/json.md",
    "docs/mcp.md",
    "docs/product-contract.md",
    "docs/storage.md",
)


class ProPriceConsistencyTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.public = root / "public"
        self.private = root / "private"
        self.binary = root / "ctx"
        self.staging = root / "stripe-staging.json"
        self.production = root / "stripe-production.json"

        write(
            self.public / "crates/ctx-cli/src/pro/pricing.rs",
            'pub(crate) const PRO_MONTHLY_PRICE_DISPLAY: &str = "$20/month";\n',
        )
        for relative in RUNTIME_SURFACES:
            write(
                self.public / relative,
                "let price = PRO_MONTHLY_PRICE_DISPLAY;\n",
            )
        for relative in PRICE_DOCS:
            write(
                self.public / relative,
                "ctx Pro is $20 USD per month.\n",
            )
        write(
            self.private
            / "services/local-pro-commercial-worker/src/referrals/model.ts",
            "export const PRO_BASE_PRICE_CENTS = 2_000;\n",
        )
        write(
            self.private / "services/local-pro-commercial-worker/wrangler.toml",
            f"""
[env.staging.vars]
STRIPE_MONTHLY_PRICE_ID = "{PRICE_ID_STAGING}"

[env.production.vars]
STRIPE_MONTHLY_PRICE_ID = "{PRICE_ID_PRODUCTION}"
""".lstrip(),
        )
        write(
            self.binary,
            """#!/bin/sh
if [ "$1" = "pro" ] && [ "$2" = "--help" ]; then
  printf '%s\n' 'Price: $20/month'
  exit 0
fi
if [ "$1" = "docs" ] && [ "$2" = "show" ] && [ "$3" = "cli-reference" ]; then
  printf '%s\n' 'ctx Pro is $20/month.'
  exit 0
fi
exit 2
""",
        )
        self.binary.chmod(0o755)
        write_json(self.staging, stripe_price(PRICE_ID_STAGING))
        write_json(self.production, stripe_price(PRICE_ID_PRODUCTION))

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def run_checker(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                "python3",
                str(CHECKER),
                "--ctx-binary",
                str(self.binary),
                "--public-root",
                str(self.public),
                "--private-root",
                str(self.private),
                "--stripe-price-json",
                str(self.staging),
                "--stripe-price-json",
                str(self.production),
            ],
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
        )

    def test_accepts_one_fixed_cross_repo_price_contract(self) -> None:
        result = self.run_checker()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("$20/month; 2000 cents", result.stdout)
        self.assertIn("2 configured Stripe prices", result.stdout)

    def test_rejects_binary_help_drift(self) -> None:
        write(
            self.binary,
            """#!/bin/sh
if [ "$1" = "pro" ]; then
  printf '%s\n' 'Price: $25/month'
else
  printf '%s\n' 'ctx Pro is $20/month.'
fi
""",
        )
        self.binary.chmod(0o755)
        result = self.run_checker()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("release binary help", result.stderr)

    def test_rejects_runtime_literal_bypass(self) -> None:
        write(
            self.public / RUNTIME_SURFACES[1],
            'let price = "$20/month"; // bypassed shared contract\n',
        )
        result = self.run_checker()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must have exactly one runtime literal owner", result.stderr)

    def test_rejects_public_doc_drift(self) -> None:
        write(
            self.public / PRICE_DOCS[0],
            "ctx Pro is $25 USD per month.\n",
        )
        result = self.run_checker()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("inconsistent monthly price", result.stderr)

    def test_rejects_private_referral_base_drift(self) -> None:
        write(
            self.private
            / "services/local-pro-commercial-worker/src/referrals/model.ts",
            "export const PRO_BASE_PRICE_CENTS = 2_500;\n",
        )
        result = self.run_checker()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("private referral base is 2500 cents", result.stderr)

    def test_rejects_configured_stripe_amount_drift(self) -> None:
        wrong = stripe_price(PRICE_ID_PRODUCTION)
        wrong["unit_amount"] = 2_500
        write_json(self.production, wrong)
        result = self.run_checker()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("is not an active USD 2000-cent", result.stderr)


def write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def write_json(path: Path, value: object) -> None:
    write(path, json.dumps(value, sort_keys=True) + "\n")


def stripe_price(price_id: str) -> dict[str, object]:
    return {
        "id": price_id,
        "object": "price",
        "active": True,
        "currency": "usd",
        "unit_amount": 2_000,
        "type": "recurring",
        "recurring": {
            "interval": "month",
            "interval_count": 1,
        },
    }


if __name__ == "__main__":
    unittest.main()
