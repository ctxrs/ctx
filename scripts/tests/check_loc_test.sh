#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
checker="$(cd "${script_dir}/.." && pwd)/check-loc.sh"
root_policy="$(cd "${script_dir}/.." && pwd)/check-loc-policy-v2.json"
scc_bin="${CTX_LOC_SCC:-}"
[[ -n "${scc_bin}" && -x "${scc_bin}" ]] || {
  printf 'check-loc test failed: CTX_LOC_SCC must name the pinned executable\n' >&2
  exit 1
}
scc_bin="$(cd "$(dirname "${scc_bin}")" && pwd)/$(basename "${scc_bin}")"

IFS=$'\t' read -r scc_version scc_archive_sha scc_binary_sha < <(
  python3 - "${root_policy}" <<'PY'
import json
import sys

metric = json.load(open(sys.argv[1], encoding="utf-8"))["metric"]
print(metric["version"], metric["archive_sha256"], metric["binary_sha256"], sep="\t")
PY
)
policy_binary_sha="${scc_binary_sha}"

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
  printf 'check-loc test failed: %s\n' "$*" >&2
}

new_case() {
  local name="$1"
  case_number=$((case_number + 1))
  current_case="${tmp}/case-${case_number}-${name}"
  snapshot=''
  policy_binary_sha="${scc_binary_sha}"
  mkdir -p "${current_case}/scripts"
  git -C "${current_case}" init -q
  git -C "${current_case}" config user.email loc-policy@example.invalid
  git -C "${current_case}" config user.name 'LOC Policy Test'
}

make_code() {
  local path="$1"
  local count="$2"
  mkdir -p "$(dirname "${path}")"
  case "${path}" in
    *.js|*.jsx|*.mjs|*.cjs|*.ts|*.tsx)
      awk -v count="${count}" 'BEGIN { for (i = 1; i <= count; i++) print "const fixture_" i " = " i ";" }' > "${path}"
      ;;
    *.bzl|*/BUILD|*/BUILD.bazel|*/MODULE.bazel)
      awk -v count="${count}" 'BEGIN { for (i = 1; i <= count; i++) print "fixture_" i " = " i }' > "${path}"
      ;;
    *)
      awk -v count="${count}" 'BEGIN { for (i = 1; i <= count; i++) print "fn fixture_" i "() {}" }' > "${path}"
      ;;
  esac
}

append_comments() {
  local path="$1"
  local count="$2"
  awk -v count="${count}" 'BEGIN { for (i = 1; i <= count; i++) print "// comment " i }' >> "${path}"
}

commit_snapshot() {
  git -C "${current_case}" add -A
  git -C "${current_case}" commit -q --allow-empty -m snapshot
  snapshot="$(git -C "${current_case}" rev-parse HEAD)"
}

write_policy() {
  local policy="${current_case}/scripts/check-loc-policy-v2.json"
  python3 - \
    "${policy}" \
    "${snapshot}" \
    "${scc_version}" \
    "${scc_archive_sha}" \
    "${policy_binary_sha}" \
    "$@" <<'PY'
import json
import sys

path, snapshot, version, archive_sha, binary_sha, *fields = sys.argv[1:]
if len(fields) % 3:
    raise SystemExit("entries must be path/kind/baseline triples")
entries = []
for index in range(0, len(fields), 3):
    entries.append(
        {
            "path": fields[index],
            "kind": fields[index + 1],
            "code_baseline": int(fields[index + 2]),
        }
    )
value = {
    "schema_version": 2,
    "policy": "Fixture policy: existing excess is frozen; checked-in ceilings ratchet on shrink.",
    "metric": {
        "tool": "scc",
        "version": version,
        "report_field": "Code",
        "archive_sha256": archive_sha,
        "binary_sha256": binary_sha,
    },
    "limits": {
        "production": {"advisory": 1000, "hard": 1500},
        "test": {"advisory": 1500, "hard": 2500},
    },
    "grandfathered_at": snapshot,
    "grandfathered": entries,
}
with open(path, "w", encoding="utf-8") as output:
    json.dump(value, output, indent=2)
    output.write("\n")
PY
  git -C "${current_case}" add scripts/check-loc-policy-v2.json
}

run_gate() {
  local output_file="${current_case}/gate-output"
  set +e
  (
    cd "${current_case}"
    CTX_LOC_POLICY_FILE=scripts/check-loc-policy-v2.json \
      CTX_LOC_SCC="${scc_bin}" \
      bash "${checker}"
  ) > "${output_file}" 2>&1
  gate_status=$?
  set -e
  gate_output="$(cat "${output_file}")"
}

expect_pass() {
  local name="$1"
  run_gate
  if ((gate_status != 0)); then
    fail "${name}: expected pass, got status ${gate_status}: ${gate_output}"
  fi
}

expect_pass_with() {
  local name="$1"
  local expected="$2"
  run_gate
  if ((gate_status != 0)); then
    fail "${name}: expected pass, got status ${gate_status}: ${gate_output}"
  elif ! grep -F -- "${expected}" <<< "${gate_output}" >/dev/null; then
    fail "${name}: output did not contain '${expected}': ${gate_output}"
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

new_case cloc-not-physical
commit_snapshot
write_policy
make_code "${current_case}/src/hard.rs" 1500
append_comments "${current_case}/src/hard.rs" 500
expect_pass 'production hard limit counts code, not physical lines'

new_case advisories
commit_snapshot
write_policy
make_code "${current_case}/src/review.rs" 1001
make_code "${current_case}/web/example.test.mjs" 2000
expect_pass_with 'advisories do not fail' 'LOC advisory report'

new_case production-hard-limit
commit_snapshot
write_policy
make_code "${current_case}/src/new.rs" 1501
expect_fail 'new production file above hard limit' '1501 CLOC > hard limit 1500'

new_case test-hard-limit
commit_snapshot
write_policy
make_code "${current_case}/tests/new.rs" 2501
expect_fail 'new test file above hard limit' '2501 CLOC > hard limit 2500'

new_case valid-grandfathered
make_code "${current_case}/src/legacy.rs" 1600
commit_snapshot
write_policy src/legacy.rs production 1600
expect_pass_with 'grandfathered hard excess' 'grandfathered ceiling 1600'

new_case grandfathered-growth
make_code "${current_case}/src/legacy.rs" 1600
commit_snapshot
write_policy src/legacy.rs production 1600
make_code "${current_case}/src/legacy.rs" 1601
expect_fail 'growth above grandfathered baseline' 'shrink-ratchet ceiling 1600'

new_case stale-shrink-ceiling
make_code "${current_case}/src/legacy.rs" 1600
commit_snapshot
write_policy src/legacy.rs production 1600
make_code "${current_case}/src/legacy.rs" 1550
expect_fail 'shrink requires lowering the checked-in ratchet' 'stale shrink-ratchet ceiling'
write_policy src/legacy.rs production 1550
expect_pass 'lowered shrink-ratchet ceiling passes'

new_case merged-shrink-ratchet
make_code "${current_case}/src/legacy.rs" 1600
commit_snapshot
write_policy src/legacy.rs production 1600
make_code "${current_case}/src/legacy.rs" 1550
write_policy src/legacy.rs production 1550
git -C "${current_case}" add -A
git -C "${current_case}" commit -q -m shrink
make_code "${current_case}/src/legacy.rs" 1551
expect_fail 'merged shrink becomes the new ceiling' 'shrink-ratchet ceiling 1550'

new_case stale-below-hard
make_code "${current_case}/src/legacy.rs" 1600
commit_snapshot
write_policy src/legacy.rs production 1600
make_code "${current_case}/src/legacy.rs" 1500
expect_fail 'entry is stale at the hard limit' 'stale grandfathered entry'

new_case new-path-cannot-be-grandfathered
commit_snapshot
make_code "${current_case}/src/new.rs" 1600
git -C "${current_case}" add src/new.rs
write_policy src/new.rs production 1600
expect_fail 'new path cannot be grandfathered' 'does not exist at'

new_case baseline-mismatch
make_code "${current_case}/src/legacy.rs" 1600
commit_snapshot
write_policy src/legacy.rs production 1599
expect_fail 'checked-in ceiling must equal current CLOC' 'shrink-ratchet ceiling 1599'

new_case duplicate-entry
make_code "${current_case}/src/legacy.rs" 1600
commit_snapshot
write_policy src/legacy.rs production 1600 src/legacy.rs production 1600
expect_fail 'duplicate grandfathered path' 'grandfathered paths must be unique'

new_case wrong-kind
make_code "${current_case}/tests/legacy.rs" 2600
commit_snapshot
write_policy tests/legacy.rs production 2600
expect_fail 'grandfathered kind follows path classification' 'kind does not match path classification'

new_case nested-starlark
commit_snapshot
write_policy
make_code "${current_case}/tools/build_defs/oversized.bzl" 1501
expect_fail 'untracked nested Starlark obeys production hard limit' '1501 CLOC > hard limit 1500'

new_case ignored-source
printf 'ignored.rs\n' > "${current_case}/.gitignore"
commit_snapshot
write_policy
make_code "${current_case}/ignored.rs" 1600
expect_pass 'ignored source is outside Git-owned inventory'

new_case scc-hash-drift
commit_snapshot
policy_binary_sha="$(printf '0%.0s' {1..64})"
write_policy
expect_fail 'runtime scc binary must match the checked-in pin' 'scc binary hash mismatch'

if ((failures > 0)); then
  exit 1
fi

printf 'check-loc policy tests passed (%s cases)\n' "${case_number}"
