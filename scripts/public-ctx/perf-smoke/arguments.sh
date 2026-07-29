#!/usr/bin/env bash

perf_smoke_fail() {
  printf 'perf smoke failed: %s\n' "$*" >&2
  exit 1
}

perf_smoke_usage() {
  cat <<'USAGE'
usage: scripts/public-ctx/perf-smoke.sh

Runs an offline public ctx CLI performance smoke against a generated Codex
session corpus. CTX_PUBLIC_CTX_REPO defaults to the checkout containing this
script.

For a comparison, set CTX_PERF_SMOKE_BASELINE_BIN to a source-authoritative
ctx executable and CTX_PERF_SMOKE_CANDIDATE_BIN to the candidate executable.
CTX_PERF_SMOKE_HEAD_BIN remains an alias for the candidate. Comparison mode
runs both baseline-first and candidate-first with isolated roots. Set
CTX_PERF_SMOKE_BIN for a single-binary diagnostic. If no binary is supplied,
the script builds and profiles target/debug/ctx from CTX_PUBLIC_CTX_REPO.

Enforced comparison mode also requires exact binary-byte bindings:
  CTX_PERF_SMOKE_BASELINE_SHA256=<lowercase 64-hex SHA-256>
  CTX_PERF_SMOKE_CANDIDATE_SHA256=<lowercase 64-hex SHA-256>

Common overrides:
  CTX_PERF_SMOKE_SESSIONS=2000
  CTX_PERF_SMOKE_LARGE_SESSION_EVENTS=4096
  CTX_PERF_SMOKE_INITIAL_REPEATS=3
  CTX_PERF_SMOKE_REPEATS=5
  CTX_PERF_SMOKE_CHANGED_FILES=5
  CTX_PERF_SMOKE_CONCURRENT_QUERIES=5
  CTX_PERF_SMOKE_COMMAND_TIMEOUT_SECONDS=300
  CTX_PERF_SMOKE_TOTAL_TIMEOUT_SECONDS=1800
  CTX_PERF_SMOKE_COMPARISON_ORDER=both
  CTX_PERF_SMOKE_REGRESSION_PCT=10
  CTX_PERF_SMOKE_MAX_PEAK_RSS_MIB=1024
  CTX_PERF_SMOKE_STATUS_P95_MS=750
  CTX_PERF_SMOKE_SEARCH_P95_MS=2500
  CTX_PERF_SMOKE_IMPORT_NOOP_P95_MS=2500
  CTX_PERF_SMOKE_IMPORT_CHANGED_P95_MS=3000
  CTX_PERF_SMOKE_IMPORT_REPLACEMENT_P95_MS=3500
  CTX_PERF_SMOKE_CONCURRENT_SEARCH_P95_MS=2500
  CTX_PERF_SMOKE_SHOW_SESSION_P95_MS=1500
  CTX_PERF_SMOKE_ENFORCE=1 (set to 0 for diagnostic-only runs)

The JSON artifact records wall and CPU time, peak RSS, Linux /proc filesystem
and device-I/O proxies, source-backed lexical/semantic/relational footprint,
and refresh-off query latency while an idempotent source rescan is active. It
includes per-order relative gate receipts. Every run uses a newly created
disposable HOME, CTX_DATA_ROOT, and generated provider tree.

Hard comparison policy is not weakenable: candidate wall and device writes
must be at or below the selected source-backed baseline, CPU and RSS must be at
or below 1.10x, device read/write/total-I/O must remain below 1.73x, and RSS
must remain at or below 1 GiB. The RSS override may only tighten that cap.
CTX_PERF_SMOKE_REGRESSION_PCT applies to the relative comparison checks.
USAGE
}

perf_smoke_find_repo_root() {
  local script_dir="$1"
  local candidate="${CTX_PUBLIC_CTX_REPO:-}"
  if [[ -z "${candidate}" ]]; then
    candidate="$(cd "${script_dir}/../.." && pwd)"
  fi
  if [[ ! -f "${candidate}/Cargo.toml" ]]; then
    perf_smoke_fail "CTX_PUBLIC_CTX_REPO does not point at a public ctx checkout: ${candidate}"
  fi
  cd "${candidate}" || perf_smoke_fail "cannot enter public ctx checkout: ${candidate}"
  pwd
}

perf_smoke_run() {
  local baseline_bin candidate_bin ctx_bin_one ctx_bin_two
  local ctx_label_one ctx_label_two head_bin_alias repo_root run_mode
  local harness_root script_dir single_bin

  script_dir="$1"
  shift

  if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    perf_smoke_usage
    return 0
  fi

  if (( "$#" > 0 )); then
    perf_smoke_usage >&2
    return 2
  fi

  command -v python3 >/dev/null 2>&1 || perf_smoke_fail 'python3 is required'

  repo_root="$(perf_smoke_find_repo_root "${script_dir}")"
  cd "${repo_root}" || perf_smoke_fail "cannot enter public ctx checkout: ${repo_root}"
  harness_root="$(cd "${script_dir}/../.." && pwd)"

  baseline_bin="${CTX_PERF_SMOKE_BASELINE_BIN:-}"
  candidate_bin="${CTX_PERF_SMOKE_CANDIDATE_BIN:-}"
  head_bin_alias="${CTX_PERF_SMOKE_HEAD_BIN:-}"
  single_bin="${CTX_PERF_SMOKE_BIN:-}"

  if [[ -n "${candidate_bin}" && -n "${head_bin_alias}" ]]; then
    perf_smoke_fail 'set CTX_PERF_SMOKE_CANDIDATE_BIN or its CTX_PERF_SMOKE_HEAD_BIN alias, not both'
  fi
  candidate_bin="${candidate_bin:-${head_bin_alias}}"

  if [[ -n "${single_bin}" && ( -n "${baseline_bin}" || -n "${candidate_bin}" ) ]]; then
    perf_smoke_fail 'set CTX_PERF_SMOKE_BIN or the baseline/candidate pair, not both'
  fi
  if [[ -n "${baseline_bin}" || -n "${candidate_bin}" ]]; then
    [[ -n "${baseline_bin}" && -n "${candidate_bin}" ]] || \
      perf_smoke_fail 'CTX_PERF_SMOKE_BASELINE_BIN and CTX_PERF_SMOKE_CANDIDATE_BIN must be set together'
    run_mode='comparison'
    ctx_bin_one="${baseline_bin}"
    ctx_label_one="${CTX_PERF_SMOKE_BASELINE_LABEL:-baseline}"
    ctx_bin_two="${candidate_bin}"
    ctx_label_two="${CTX_PERF_SMOKE_CANDIDATE_LABEL:-${CTX_PERF_SMOKE_HEAD_LABEL:-candidate}}"
  else
    run_mode='single'
    ctx_bin_one="${single_bin}"
    ctx_label_one="${CTX_PERF_SMOKE_LABEL:-current}"
    ctx_bin_two=''
    ctx_label_two=''
  fi

  if [[ -z "${ctx_bin_one}" ]]; then
    printf '==> cargo build --quiet --locked -p ctx --bin ctx\n'
    cargo build --quiet --locked -p ctx --bin ctx
    ctx_bin_one="${repo_root}/target/debug/ctx"
  fi

  [[ -x "${ctx_bin_one}" ]] || perf_smoke_fail "ctx binary is not executable: ${ctx_bin_one}"
  if [[ -n "${ctx_bin_two}" ]]; then
    [[ -x "${ctx_bin_two}" ]] || perf_smoke_fail "ctx binary is not executable: ${ctx_bin_two}"
  fi

  perf_smoke_python_source | python3 - \
    "${repo_root}" "${harness_root}" "${run_mode}" \
    "${ctx_bin_one}" "${ctx_label_one}" "${ctx_bin_two}" "${ctx_label_two}"
}

perf_smoke_emit_python_arguments() {
  cat <<'PY'
from __future__ import annotations

import datetime as dt
import hashlib
import json
import math
import os
import re
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path


REPO_ROOT = Path(sys.argv[1]).resolve()
HARNESS_ROOT = Path(sys.argv[2]).resolve()
RUN_MODE = sys.argv[3]
RUN_SPECS = [
    (Path(sys.argv[4]).resolve(), sys.argv[5], "single"),
]
if RUN_MODE == "comparison":
    RUN_SPECS = [
        (Path(sys.argv[4]).resolve(), sys.argv[5], "baseline"),
        (Path(sys.argv[6]).resolve(), sys.argv[7], "candidate"),
    ]
QUERY = "perfneedle"
PROC_IO_FIELDS = ("rchar", "wchar", "read_bytes", "write_bytes", "cancelled_write_bytes")
HARNESS_STARTED = time.perf_counter()
CORE_PHASES = (
    "initial_import",
    "noop_incremental_import",
    "append_incremental_import",
    "replacement_import",
)
WALL_PARITY_RATIO = 1.0
DEVICE_WRITE_PARITY_RATIO = 1.0
CPU_RSS_RELATIVE_RATIO = 1.10
IO_AMPLIFICATION_RATIO = 1.73
EXPECTED_SHA256_ENV_BY_ROLE = {
    "baseline": "CTX_PERF_SMOKE_BASELINE_SHA256",
    "candidate": "CTX_PERF_SMOKE_CANDIDATE_SHA256",
}


class HarnessError(Exception):
    pass


def env_flag(name: str, default: bool) -> bool:
    raw = os.environ.get(name)
    if raw is None:
        return default
    return raw.strip().lower() not in {"", "0", "false", "no", "off"}


def env_int(
    name: str,
    default: int,
    minimum: int = 1,
    maximum: int | None = None,
) -> int:
    raw = os.environ.get(name)
    if raw is None:
        return default
    try:
        value = int(raw)
    except ValueError as exc:
        raise HarnessError(f"{name} must be an integer, got {raw!r}") from exc
    if value < minimum:
        raise HarnessError(f"{name} must be at least {minimum}, got {value}")
    if maximum is not None and value > maximum:
        raise HarnessError(f"{name} must be at most {maximum}, got {value}")
    return value


def env_float(
    name: str,
    default: float,
    minimum: float = 0.0,
    maximum: float | None = None,
) -> float:
    raw = os.environ.get(name)
    if raw is None:
        return default
    try:
        value = float(raw)
    except ValueError as exc:
        raise HarnessError(f"{name} must be a number, got {raw!r}") from exc
    if not math.isfinite(value):
        raise HarnessError(f"{name} must be finite, got {raw!r}")
    if value < minimum:
        raise HarnessError(f"{name} must be at least {minimum}, got {value}")
    if maximum is not None and value > maximum:
        raise HarnessError(f"{name} must be at most {maximum}, got {value}")
    return value


PY
}
