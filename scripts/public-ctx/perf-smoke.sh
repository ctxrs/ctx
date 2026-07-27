#!/usr/bin/env bash
set -euo pipefail

perf_smoke_script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=scripts/public-ctx/perf-smoke/harness.sh
source "${perf_smoke_script_dir}/perf-smoke/harness.sh"

perf_smoke_run "${perf_smoke_script_dir}" "$@"
