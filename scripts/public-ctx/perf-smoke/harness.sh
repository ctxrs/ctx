#!/usr/bin/env bash

perf_smoke_helper_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=scripts/public-ctx/perf-smoke/arguments.sh
source "${perf_smoke_helper_dir}/arguments.sh"
# shellcheck source=scripts/public-ctx/perf-smoke/fixtures.sh
source "${perf_smoke_helper_dir}/fixtures.sh"
# shellcheck source=scripts/public-ctx/perf-smoke/metrics.sh
source "${perf_smoke_helper_dir}/metrics.sh"
# shellcheck source=scripts/public-ctx/perf-smoke/validation.sh
source "${perf_smoke_helper_dir}/validation.sh"
# shellcheck source=scripts/public-ctx/perf-smoke/runner.sh
source "${perf_smoke_helper_dir}/runner.sh"
# shellcheck source=scripts/public-ctx/perf-smoke/report.sh
source "${perf_smoke_helper_dir}/report.sh"
# shellcheck source=scripts/public-ctx/perf-smoke/entrypoint.sh
source "${perf_smoke_helper_dir}/entrypoint.sh"

unset perf_smoke_helper_dir

# Keep the original Python definition order stable. Besides preserving behavior,
# this makes tracebacks and the frozen source digest useful during refactors.
perf_smoke_python_source() {
  perf_smoke_emit_python_arguments
  perf_smoke_emit_python_statistics
  perf_smoke_emit_python_fixtures
  perf_smoke_emit_python_metrics
  perf_smoke_emit_python_validation_oracles
  perf_smoke_emit_python_runner
  perf_smoke_emit_python_validation_policy
  perf_smoke_emit_python_report
  perf_smoke_emit_python_entrypoint
}
