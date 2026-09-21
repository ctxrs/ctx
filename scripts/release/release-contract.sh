#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
# Signed publication integrity is mandatory. Native installer and stock-client
# upgrade proofs have separate owning commands/receipts and are never inferred
# from this readback. The approved 1.5 route does not run release campaigns.
exec node scripts/release/release-contract.cjs
