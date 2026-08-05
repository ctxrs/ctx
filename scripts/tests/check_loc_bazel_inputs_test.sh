#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" ]]; then
  input_root="${TEST_SRCDIR}/${TEST_WORKSPACE:-_main}"
else
  input_root="$(git rev-parse --show-toplevel)"
fi
scc_bin="${1:-${CTX_LOC_SCC:-}}"
manifest="${2:-${CTX_LOC_PATHS_MANIFEST:-}}"

assert_reaches_gate() {
  local path="$1"
  local bytes
  [[ -f "${input_root}/${path}" ]] || {
    printf 'LOC Bazel input contract failed: missing runfile %s\n' "${path}" >&2
    exit 1
  }
  bytes="$(wc -c < "${input_root}/${path}" | tr -d '[:space:]')"
  ((bytes > 0)) || {
    printf 'LOC Bazel input contract failed: empty runfile %s\n' "${path}" >&2
    exit 1
  }
}

assert_reaches_gate crates/ctx-history-capture/src/provider/source_backed/driver.rs
assert_reaches_gate tools/bazel/release_routes_test.bzl
assert_reaches_gate scripts/check-loc.py
assert_reaches_gate scripts/check-loc-policy-v2.json

[[ -n "${scc_bin}" && -x "${scc_bin}" ]] || {
  printf 'LOC Bazel input contract failed: pinned scc runfile is unavailable: %s\n' "${scc_bin:-<unset>}" >&2
  exit 1
}
[[ -n "${manifest}" && -f "${manifest}" ]] || {
  printf 'LOC Bazel input contract failed: declared-source manifest is unavailable\n' >&2
  exit 1
}
if grep -Eq '^(bazel-[^/]*/|external/|target/)' "${manifest}"; then
  printf 'LOC Bazel input contract failed: generated output entered source runfiles\n' >&2
  exit 1
fi
for path in \
  crates/ctx-history-capture/src/provider/source_backed/driver.rs \
  scripts/source-backed-recovery/fault_shim.c \
  scripts/source-backed-recovery/run-bazel-linux-fault-test.sh \
  tools/bazel/release_routes_test.bzl \
  scripts/check-loc.py; do
  grep -Fxq "${path}" "${manifest}" || {
    printf 'LOC Bazel input contract failed: declared-source manifest omits %s\n' "${path}" >&2
    exit 1
  }
done

IFS=$'\t' read -r version archive_sha binary_sha < <(
  python3 - "${input_root}/scripts/check-loc-policy-v2.json" <<'PY'
import json
import sys

metric = json.load(open(sys.argv[1], encoding="utf-8"))["metric"]
print(metric["version"], metric["archive_sha256"], metric["binary_sha256"], sep="\t")
PY
)
actual_version="$("${scc_bin}" --version)"
[[ "${actual_version}" == "scc version ${version}" ]] || {
  printf 'LOC Bazel input contract failed: scc version mismatch: %s\n' "${actual_version}" >&2
  exit 1
}
actual_binary_sha="$(python3 - "${scc_bin}" <<'PY'
import hashlib
import sys

print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())
PY
)"
[[ "${actual_binary_sha}" == "${binary_sha}" ]] || {
  printf 'LOC Bazel input contract failed: scc binary hash mismatch\n' >&2
  exit 1
}
grep -Fq "sha256 = \"${archive_sha}\"" "${input_root}/MODULE.bazel" || {
  printf 'LOC Bazel input contract failed: MODULE.bazel does not carry the policy archive hash\n' >&2
  exit 1
}
grep -Fq "/v${version}/scc_Linux_x86_64.tar.gz" "${input_root}/MODULE.bazel" || {
  printf 'LOC Bazel input contract failed: MODULE.bazel does not carry the policy scc version\n' >&2
  exit 1
}

printf 'LOC Bazel input contract passed (source inventory and pinned scc 3.7.0 present).\n'
