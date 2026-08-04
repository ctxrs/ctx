#!/usr/bin/env python3
"""Validate the public exact MCP tool-call attribution evidence contract."""

from __future__ import annotations

import sys

sys.dont_write_bytecode = True

from check_mcp_tool_call_attribution_capabilities_lib import (
    CapabilityError,
    load_contract,
    validate_contract,
)


def main() -> int:
    try:
        support, capability, docs = load_contract()
        result = validate_contract(support, capability, docs)
    except (CapabilityError, OSError) as exc:
        print(f"MCP tool-call attribution capability check failed: {exc}", file=sys.stderr)
        return 1

    print(
        "MCP tool-call attribution capabilities ok: "
        f"providers={result['providers']} base_routes={result['base_routes']} "
        f"capability_lanes={result['capability_lanes']} "
        f"lane_statuses={result['lane_statuses']} "
        f"provider_statuses={result['provider_statuses']} "
        f"exact_suites={result['exact_suites']} exact_tests={result['exact_tests']} "
        f"exact_links={result['exact_links']} "
        f"conformance_authority={result['conformance_authority']}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
