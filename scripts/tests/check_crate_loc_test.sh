#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
checker="$(cd "${script_dir}/.." && pwd)/check-crate-loc.sh"
root_metric_policy="$(cd "${script_dir}/.." && pwd)/check-loc-policy-v2.json"
scc_bin="${CTX_CRATE_LOC_SCC:-${CTX_LOC_SCC:-}}"
[[ -n "${scc_bin}" && -x "${scc_bin}" ]] || {
  printf 'crate LOC test failed: CTX_CRATE_LOC_SCC must name the pinned executable\n' >&2
  exit 1
}
scc_bin="$(cd "$(dirname "${scc_bin}")" && pwd)/$(basename "${scc_bin}")"

tmp="$(mktemp -d)"
trap 'rm -rf -- "${tmp}"' EXIT

case_number=0
failures=0
current_case=''
snapshot=''
gate_status=0
gate_output=''

fail() {
  failures=$((failures + 1))
  printf 'crate LOC test failed: %s\n' "$*" >&2
}

new_case() {
  local name="$1"
  case_number=$((case_number + 1))
  current_case="${tmp}/case-${case_number}-${name}"
  snapshot=''
  mkdir -p "${current_case}/scripts"
  git -C "${current_case}" init -q
  git -C "${current_case}" config user.email crate-loc@example.invalid
  git -C "${current_case}" config user.name 'Crate LOC Test'
  cp "${root_metric_policy}" "${current_case}/scripts/check-loc-policy-v2.json"
}

write_workspace() {
  local package="$1"
  local package_dir="$2"
  mkdir -p "${current_case}/${package_dir}/src"
  {
    printf '[workspace]\n'
    printf 'members = ["%s"]\n' "${package_dir}"
    printf 'resolver = "2"\n'
  } > "${current_case}/Cargo.toml"
  {
    printf '[package]\n'
    printf 'name = "%s"\n' "${package}"
    printf 'version = "0.1.0"\n'
    printf 'edition = "2021"\n'
  } > "${current_case}/${package_dir}/Cargo.toml"
}

make_code() {
  local path="$1"
  local count="$2"
  mkdir -p "$(dirname "${path}")"
  awk -v count="${count}" 'BEGIN { for (i = 1; i <= count; i++) print "fn fixture_" i "() {}" }' > "${path}"
}

commit_snapshot() {
  git -C "${current_case}" add -A
  git -C "${current_case}" commit -q --allow-empty -m snapshot
  snapshot="$(git -C "${current_case}" rev-parse HEAD)"
}

write_policy() {
  local package="${1:-}"
  local manifest="${2:-}"
  local baseline="${3:-}"
  python3 - "${current_case}/scripts/check-crate-loc-policy-v1.json" "${snapshot}" "${package}" "${manifest}" "${baseline}" <<'PY'
import json
import sys

path, snapshot, package, manifest, baseline = sys.argv[1:]
entries = []
if package:
    entries.append(
        {
            "package": package,
            "manifest": manifest,
            "code_baseline": int(baseline),
        }
    )
value = {
    "schema_version": 1,
    "policy": "Fixture hard crate limit with shrink-only migration entries.",
    "metric_policy": "scripts/check-loc-policy-v2.json",
    "hard_limit": 20000,
    "grandfathered_at": snapshot,
    "grandfathered": entries,
}
with open(path, "w", encoding="utf-8") as output:
    json.dump(value, output, indent=2)
    output.write("\n")
PY
  git -C "${current_case}" add scripts/check-crate-loc-policy-v1.json
}

run_gate() {
  local output_file="${current_case}/gate-output"
  set +e
  (
    cd "${current_case}"
    CTX_CRATE_LOC_SCC="${scc_bin}" bash "${checker}"
  ) > "${output_file}" 2>&1
  gate_status=$?
  set -e
  gate_output="$(cat "${output_file}")"
}

expect_pass() {
  local name="$1"
  run_gate
  if ((gate_status != 0)); then
    fail "${name}: expected pass, got ${gate_status}: ${gate_output}"
  fi
}

expect_fail() {
  local name="$1"
  local expected="$2"
  run_gate
  if ((gate_status == 0)); then
    fail "${name}: expected failure"
  elif ! grep -F -- "${expected}" <<< "${gate_output}" >/dev/null; then
    fail "${name}: output did not contain '${expected}': ${gate_output}"
  fi
}

new_case exact-limit-target-aware
write_workspace exact crates/exact
make_code "${current_case}/crates/exact/src/lib.rs" 19996
make_code "${current_case}/crates/exact/src/generated.rs" 2
make_code "${current_case}/crates/exact/build.rs" 2
make_code "${current_case}/crates/exact/src/tests.rs" 500
make_code "${current_case}/crates/exact/src/nested/tests/fixture.rs" 500
make_code "${current_case}/crates/exact/src/ignored_tests.rs" 500
make_code "${current_case}/crates/exact/src/test_support.rs" 500
commit_snapshot
write_policy
expect_pass 'exact 20k includes build script and checked-in generated Rust but excludes test modules'

new_case hard-limit
write_workspace excess crates/excess
make_code "${current_case}/crates/excess/src/lib.rs" 20001
commit_snapshot
write_policy
expect_fail 'new crate above 20k' 'excess: 20001 CLOC > hard limit 20000'

new_case valid-ledger
write_workspace legacy crates/legacy
make_code "${current_case}/crates/legacy/src/lib.rs" 21000
commit_snapshot
write_policy legacy crates/legacy/Cargo.toml 21000
expect_pass 'exact no-growth baseline'

new_case ledger-growth
write_workspace legacy crates/legacy
make_code "${current_case}/crates/legacy/src/lib.rs" 21000
commit_snapshot
write_policy legacy crates/legacy/Cargo.toml 21000
make_code "${current_case}/crates/legacy/src/lib.rs" 21001
expect_fail 'grandfathered growth' 'legacy: 21001 CLOC > no-growth ceiling 21000'

new_case ledger-shrink
write_workspace legacy crates/legacy
make_code "${current_case}/crates/legacy/src/lib.rs" 21000
commit_snapshot
write_policy legacy crates/legacy/Cargo.toml 21000
make_code "${current_case}/crates/legacy/src/lib.rs" 20999
expect_fail 'shrink must lower checked-in ceiling' 'stale no-growth ceiling 21000; lower it to current 20999 CLOC'

new_case stale-at-limit
write_workspace legacy crates/legacy
make_code "${current_case}/crates/legacy/src/lib.rs" 21000
commit_snapshot
write_policy legacy crates/legacy/Cargo.toml 21000
make_code "${current_case}/crates/legacy/src/lib.rs" 20000
expect_fail 'entry disappears at hard limit' 'stale grandfathered entry at 20000 CLOC'

new_case new-exception-forbidden
write_workspace legacy crates/legacy
make_code "${current_case}/crates/legacy/src/lib.rs" 19000
commit_snapshot
make_code "${current_case}/crates/legacy/src/lib.rs" 21000
write_policy legacy crates/legacy/Cargo.toml 21000
expect_fail 'new exception cannot rewrite the snapshot' 'grandfathered baseline mismatch'

new_case deterministic-report
write_workspace stable crates/stable
make_code "${current_case}/crates/stable/src/lib.rs" 17
commit_snapshot
write_policy
run_gate
first="${gate_output}"
run_gate
second="${gate_output}"
if [[ "${first}" != "${second}" ]]; then
  fail 'deterministic report changed between identical runs'
fi

if ((failures > 0)); then
  exit 1
fi

printf 'crate LOC policy tests passed (%s cases)\n' "${case_number}"
