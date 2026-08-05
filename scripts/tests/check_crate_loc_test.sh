#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
checker="$(cd "${script_dir}/.." && pwd)/check-crate-loc.sh"
scc_bin="${CTX_CRATE_LOC_SCC:-${CTX_LOC_SCC:-}}"
crate_root="${CTX_CRATE_LOC_ROOT:-$(cd "${script_dir}/../.." && pwd)}"
crate_manifest="${CTX_CRATE_LOC_PATHS_MANIFEST:-}"
record_value="${CTX_CRATE_LOC_BAZEL_RECORDS:-}"

[[ -n "${scc_bin}" && -x "${scc_bin}" ]] || {
  printf 'crate gate test failed: CTX_CRATE_LOC_SCC must name the pinned executable\n' >&2
  exit 1
}
[[ -n "${record_value}" ]] || {
  printf 'crate gate test failed: CTX_CRATE_LOC_BAZEL_RECORDS is required\n' >&2
  exit 1
}

tmp="$(mktemp -d)"
trap 'rm -rf -- "${tmp}"' EXIT

python3 "${script_dir}/check_crate_loc_unit_test.py"

canonical_report() {
  local path="$1"
  local expected_status="$2"
  python3 - "${path}" "${expected_status}" <<'PY'
import json
from pathlib import Path
import sys

path = Path(sys.argv[1])
expected = sys.argv[2]
raw = path.read_text(encoding="utf-8")
if raw.count("\n") != 1:
    raise SystemExit(f"stdout must contain exactly one JSON line, got {raw.count(chr(10))}")
value = json.loads(raw)
canonical = json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n"
if raw != canonical:
    raise SystemExit("gate stdout is not canonical JSON")
if value.get("status") != expected:
    raise SystemExit(f"expected status {expected}, got {value.get('status')}")
required = {"schema_version", "status", "metric", "platforms", "targets", "source_digest", "cloc", "graph", "violations"}
if set(value) != required:
    raise SystemExit(f"canonical report keys changed: {sorted(value)}")
PY
}

run_gate() {
  local records="$1"
  local stdout="$2"
  local stderr="$3"
  local -a environment=(
    "CTX_CRATE_LOC_BAZEL_RECORDS=${records}"
    "CTX_CRATE_LOC_ROOT=${crate_root}"
    "CTX_CRATE_LOC_SCC=${scc_bin}"
  )
  if [[ -n "${crate_manifest}" ]]; then
    environment+=("CTX_CRATE_LOC_PATHS_MANIFEST=${crate_manifest}")
  fi
  env "${environment[@]}" bash "${checker}" >"${stdout}" 2>"${stderr}"
}

run_gate "${record_value}" "${tmp}/pass-1.json" "${tmp}/pass-1.err"
canonical_report "${tmp}/pass-1.json" pass

# A direct checkout and a declared-runfiles invocation must produce identical
# canonical bytes whenever both source authorities are available.
if [[ -n "${crate_manifest}" ]] && git -C "${crate_root}" rev-parse --show-toplevel >/dev/null 2>&1; then
  env \
    CTX_CRATE_LOC_BAZEL_RECORDS="${record_value}" \
    CTX_CRATE_LOC_ROOT="${crate_root}" \
    CTX_CRATE_LOC_SCC="${scc_bin}" \
    bash "${checker}" >"${tmp}/direct.json" 2>"${tmp}/direct.err"
  cmp "${tmp}/pass-1.json" "${tmp}/direct.json"
fi

IFS=: read -r -a records <<<"${record_value}"
cp "${records[0]}" "${tmp}/malformed.tsv"
chmod u+w "${tmp}/malformed.tsv"
printf 'malformed\n' >>"${tmp}/malformed.tsv"
set +e
run_gate "${tmp}/malformed.tsv" "${tmp}/failure.json" "${tmp}/failure.err"
failure_status=$?
set -e
((failure_status != 0)) || { printf 'crate gate test failed: malformed action inventory passed\n' >&2; exit 1; }
canonical_report "${tmp}/failure.json" fail

printf 'crate source/graph gate tests passed (20 unit cases, canonical pass/failure, action/edge mutations)\n'
