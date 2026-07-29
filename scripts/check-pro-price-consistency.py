#!/usr/bin/env python3
"""Verify the fixed Pro price across a release binary and private billing inputs."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path
from typing import Any


PRICE_SOURCE = Path("crates/ctx-cli/src/pro/pricing.rs")
RUNTIME_PRICE_SURFACES = (
    Path("crates/ctx-cli/src/cli.rs"),
    Path("crates/ctx-cli/src/commands/status.rs"),
    Path("crates/ctx-cli/src/local_usage/report.rs"),
    Path("crates/ctx-cli/src/pro/commercial_lifecycle.rs"),
    Path("crates/ctx-cli/src/pro/lifecycle_commands.rs"),
)
PRICE_DOCS = (
    Path("docs/cli-reference.md"),
    Path("docs/contracts/json.md"),
    Path("docs/mcp.md"),
    Path("docs/product-contract.md"),
    Path("docs/storage.md"),
)
PRIVATE_BASE_SOURCE = Path(
    "services/local-pro-commercial-worker/src/referrals/model.ts"
)
PRIVATE_DEPLOY_CONFIG = Path("services/local-pro-commercial-worker/wrangler.toml")
DISPLAY_CONSTANT = "PRO_MONTHLY_PRICE_DISPLAY"


class ContractError(RuntimeError):
    pass


def read_text(root: Path, relative: Path) -> str:
    path = root / relative
    try:
        return path.read_text(encoding="utf-8")
    except OSError as error:
        raise ContractError(f"could not read {path}: {error}") from error


def public_price_contract(public_root: Path) -> tuple[str, int]:
    source = read_text(public_root, PRICE_SOURCE)
    match = re.search(
        rf"{DISPLAY_CONSTANT}\s*:\s*&str\s*=\s*\"(\$[0-9]+/month)\"\s*;",
        source,
    )
    if match is None:
        raise ContractError(
            f"{PRICE_SOURCE} must define one literal {DISPLAY_CONSTANT}"
        )
    display = match.group(1)
    dollars_match = re.fullmatch(r"\$([0-9]+)/month", display)
    if dollars_match is None:
        raise ContractError(f"invalid monthly display price: {display}")
    amount_cents = int(dollars_match.group(1)) * 100

    literal_owners: list[Path] = []
    source_root = public_root / "crates/ctx-cli/src"
    for path in source_root.rglob("*.rs"):
        relative = path.relative_to(public_root)
        if path.name == "tests.rs" or "tests" in path.parts:
            continue
        text = path.read_text(encoding="utf-8")
        if display in text:
            literal_owners.append(relative)
    if literal_owners != [PRICE_SOURCE]:
        rendered = ", ".join(str(path) for path in literal_owners) or "none"
        raise ContractError(
            f"{display} must have exactly one runtime literal owner; found {rendered}"
        )

    for relative in RUNTIME_PRICE_SURFACES:
        text = read_text(public_root, relative)
        if DISPLAY_CONSTANT not in text:
            raise ContractError(
                f"{relative} must consume {DISPLAY_CONSTANT}"
            )
        if relative != Path("crates/ctx-cli/src/cli.rs") and re.search(
            r"\$[0-9]+/month",
            text,
        ):
            raise ContractError(
                f"{relative} contains a monthly price literal instead of "
                f"{DISPLAY_CONSTANT}"
            )
    return display, amount_cents


def validate_public_docs(public_root: Path, amount_cents: int) -> None:
    expected_dollars = amount_cents // 100
    matched_paths: set[Path] = set()
    docs_root = public_root / "docs"
    for path in docs_root.rglob("*.md"):
        relative = path.relative_to(public_root)
        text = path.read_text(encoding="utf-8")
        for match in re.finditer(
            r"\$(?P<dollars>[0-9]+)(?: USD)?"
            r"(?P<cadence>/month| per month| monthly)",
            text,
        ):
            dollars = int(match.group("dollars"))
            if dollars == expected_dollars:
                matched_paths.add(relative)
                continue
            context = text[max(0, match.start() - 100) : match.end() + 100].lower()
            if dollars == 10 and any(
                marker in context for marker in ("commission", "earn", "referral")
            ):
                continue
            raise ContractError(
                f"{relative} contains inconsistent monthly price "
                f"{match.group(0)!r}"
            )
    missing = sorted(set(PRICE_DOCS) - matched_paths)
    if missing:
        raise ContractError(
            "release-scope pricing docs do not contain the fixed Pro price: "
            + ", ".join(str(path) for path in missing)
        )


def validate_binary_help(binary: Path, display: str) -> None:
    if not binary.is_file():
        raise ContractError(f"ctx binary is not a file: {binary}")
    try:
        result = subprocess.run(
            [str(binary), "pro", "--help"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=15,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ContractError(f"could not execute ctx Pro help: {error}") from error
    if result.returncode != 0:
        detail = result.stderr.strip()
        raise ContractError(
            f"ctx pro --help failed with exit {result.returncode}: {detail}"
        )
    if f"Price: {display}" not in result.stdout:
        raise ContractError(
            f"release binary help does not advertise Price: {display}"
        )
    try:
        result = subprocess.run(
            [str(binary), "docs", "show", "cli-reference"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=15,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ContractError(
            f"could not read release binary pricing docs: {error}"
        ) from error
    if result.returncode != 0:
        detail = result.stderr.strip()
        raise ContractError(
            "ctx docs show cli-reference failed with exit "
            f"{result.returncode}: {detail}"
        )
    if display not in result.stdout:
        raise ContractError(
            f"release binary embedded docs do not contain {display}"
        )


def private_base_price_cents(private_root: Path) -> int:
    source = read_text(private_root, PRIVATE_BASE_SOURCE)
    match = re.search(
        r"PRO_BASE_PRICE_CENTS\s*=\s*([0-9][0-9_]*)\s*;",
        source,
    )
    if match is None:
        raise ContractError(
            f"{PRIVATE_BASE_SOURCE} does not define PRO_BASE_PRICE_CENTS"
        )
    return int(match.group(1).replace("_", ""))


def configured_stripe_price_ids(private_root: Path) -> set[str]:
    path = private_root / PRIVATE_DEPLOY_CONFIG
    try:
        parsed = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise ContractError(f"could not parse {path}: {error}") from error
    ids: set[str] = set()
    try:
        environments = parsed["env"]
        for channel in ("staging", "production"):
            value = environments[channel]["vars"]["STRIPE_MONTHLY_PRICE_ID"]
            if not isinstance(value, str) or not value.startswith("price_"):
                raise KeyError(channel)
            ids.add(value)
    except (KeyError, TypeError) as error:
        raise ContractError(
            f"{PRIVATE_DEPLOY_CONFIG} must configure staging and production "
            "STRIPE_MONTHLY_PRICE_ID values"
        ) from error
    if len(ids) != 2:
        raise ContractError("staging and production must use distinct Stripe Price IDs")
    return ids


def load_json_object(path: Path) -> dict[str, Any]:
    try:
        value: Any = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ContractError(f"could not parse Stripe Price evidence {path}: {error}") from error
    if not isinstance(value, dict):
        raise ContractError(f"Stripe Price evidence is not an object: {path}")
    return value


def validate_stripe_prices(
    evidence_paths: list[Path],
    configured_ids: set[str],
    amount_cents: int,
) -> None:
    evidence_ids: set[str] = set()
    for path in evidence_paths:
        price = load_json_object(path)
        price_id = price.get("id")
        recurring = price.get("recurring")
        if not isinstance(price_id, str):
            raise ContractError(f"Stripe Price evidence has no string id: {path}")
        if price_id in evidence_ids:
            raise ContractError(f"duplicate Stripe Price evidence for {price_id}")
        evidence_ids.add(price_id)
        if (
            price.get("object") != "price"
            or price.get("active") is not True
            or price.get("currency") != "usd"
            or price.get("unit_amount") != amount_cents
            or price.get("type") != "recurring"
            or not isinstance(recurring, dict)
            or recurring.get("interval") != "month"
            or recurring.get("interval_count") != 1
        ):
            raise ContractError(
                f"Stripe Price {price_id} is not an active USD "
                f"{amount_cents}-cent monthly recurring price"
            )
    if evidence_ids != configured_ids:
        missing = sorted(configured_ids - evidence_ids)
        extra = sorted(evidence_ids - configured_ids)
        raise ContractError(
            "Stripe Price evidence does not match checked-in configuration "
            f"(missing={missing}, extra={extra})"
        )


def parse_args(arguments: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ctx-binary", type=Path, required=True)
    parser.add_argument(
        "--public-root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
    )
    parser.add_argument("--private-root", type=Path, required=True)
    parser.add_argument(
        "--stripe-price-json",
        type=Path,
        action="append",
        required=True,
        help="Stripe Price API JSON evidence; pass once per configured Price ID",
    )
    return parser.parse_args(arguments)


def main(arguments: list[str] | None = None) -> int:
    parsed = parse_args(sys.argv[1:] if arguments is None else arguments)
    try:
        public_root = parsed.public_root.resolve(strict=True)
        private_root = parsed.private_root.resolve(strict=True)
        binary = parsed.ctx_binary.resolve(strict=True)
        evidence = [
            path.resolve(strict=True) for path in parsed.stripe_price_json
        ]
        display, amount_cents = public_price_contract(public_root)
        validate_public_docs(public_root, amount_cents)
        validate_binary_help(binary, display)
        private_base = private_base_price_cents(private_root)
        if private_base != amount_cents:
            raise ContractError(
                f"private referral base is {private_base} cents; "
                f"public Pro price is {amount_cents} cents"
            )
        configured_ids = configured_stripe_price_ids(private_root)
        validate_stripe_prices(evidence, configured_ids, amount_cents)
    except (ContractError, OSError) as error:
        print(f"Pro price consistency check failed: {error}", file=sys.stderr)
        return 1
    print(
        "Pro price consistency ok: "
        f"{display}; {amount_cents} cents; "
        f"{len(configured_ids)} configured Stripe prices; "
        f"private referral base {private_base} cents"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
