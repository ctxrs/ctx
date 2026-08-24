#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
checker="${repo_root}/scripts/check-public-control-surface.py"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

fixture="${tmp}/fixture"
mkdir -p \
  "${fixture}/contracts" \
  "${fixture}/contracts/stable-defaults" \
  "${fixture}/crates/ctx-cli/src" \
  "${fixture}/crates/ctx-client-observability/src/analytics" \
  "${fixture}/crates/ctx-upgrade-engine/tests/contracts" \
  "${fixture}/crates/ctx-upgrade-engine/src/upgrade" \
  "${fixture}/scripts" \
  "${fixture}/docs"
cp "${repo_root}/contracts/public-control-surface-v1.json" "${fixture}/contracts/"
cp "${repo_root}/contracts/stable-defaults/v0.25.0.json" \
  "${fixture}/contracts/stable-defaults/"
cp "${repo_root}/crates/ctx-cli/src/config.rs" "${fixture}/crates/ctx-cli/src/"
cp "${repo_root}/crates/ctx-cli/src/deprecated_controls.rs" "${fixture}/crates/ctx-cli/src/"
cp "${repo_root}/crates/ctx-client-observability/src/analytics/operation.rs" \
  "${fixture}/crates/ctx-client-observability/src/analytics/"
cp "${repo_root}/crates/ctx-cli/src/config_tests.rs" "${fixture}/crates/ctx-cli/src/"
cp "${repo_root}/crates/ctx-cli/src/process_environment.rs" "${fixture}/crates/ctx-cli/src/"
cp "${repo_root}/crates/ctx-upgrade-engine/src/upgrade/metadata.rs" \
  "${fixture}/crates/ctx-upgrade-engine/src/upgrade/"
cp "${repo_root}/crates/ctx-upgrade-engine/tests/contracts/upgrade.rs" \
  "${fixture}/crates/ctx-upgrade-engine/tests/contracts/"
cp "${repo_root}/scripts/smoke-daemon-semantic-release.ps1" "${fixture}/scripts/"
cp "${repo_root}/scripts/smoke-daemon-semantic-release.sh" "${fixture}/scripts/"
cp "${repo_root}/scripts/test-windows-legacy-daemon-takeover.ps1" "${fixture}/scripts/"
cp "${repo_root}/docs/storage.md" "${fixture}/docs/"

python3 "${checker}" "${fixture}" > "${tmp}/pass.out"
grep -Fq '5 empty-config released defaults' "${tmp}/pass.out"

mkdir "${tmp}/no-git-bin"
ln -s "$(command -v python3)" "${tmp}/no-git-bin/python3"
PATH="${tmp}/no-git-bin" python3 "${checker}" "${fixture}" > "${tmp}/no-git.out"
grep -Fq '5 empty-config released defaults' "${tmp}/no-git.out"

expect_fail() {
  local name="$1"
  local expected="$2"
  local case_root="${tmp}/${name}"
  cp -R "${fixture}" "${case_root}"
  shift 2
  "$@" "${case_root}"
  if python3 "${checker}" "${case_root}" > "${tmp}/${name}.out" 2>&1; then
    printf 'checker unexpectedly accepted %s\n' "${name}" >&2
    exit 1
  fi
  grep -Fq "${expected}" "${tmp}/${name}.out"
}

change_inventory_default() {
  sed -i '0,/"value": true/{s/"value": true/"value": false/}' \
    "$1/contracts/public-control-surface-v1.json"
}

make_unapproved_change() {
  sed -i '0,/enabled: true/{s/enabled: true/enabled: false/}' \
    "$1/crates/ctx-cli/src/config.rs"
  python3 - "$1/contracts/public-control-surface-v1.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
contract = json.loads(path.read_text())
analytics = next(
    control for control in contract["controls"]
    if control["config_key"] == "analytics.enabled"
)
analytics["released_default"] = {
    "value": False,
    "state": "off",
    "scope": "all_cli_installations",
}
path.write_text(json.dumps(contract, indent=2) + "\n")
PY
}

add_unscoped_evidence() {
  make_unapproved_change "$1"
  python3 - "$1/contracts/public-control-surface-v1.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
contract = json.loads(path.read_text())
analytics = next(
    control for control in contract["controls"]
    if control["config_key"] == "analytics.enabled"
)
analytics["deliberate_change_approval"] = {
    "reason": "test-only deliberate change",
    "evidence_commits": ["0123456789abcdef0123456789abcdef01234567"],
}
path.write_text(json.dumps(contract, indent=2) + "\n")
PY
}

change_runtime_default() {
  sed -i 's/AUTO_UPGRADE_DEFAULT_MODE: &str = "apply"/AUTO_UPGRADE_DEFAULT_MODE: \&str = "off"/' \
    "$1/crates/ctx-cli/src/config.rs"
}

rewrite_history_to_hide_a_regression() {
  sed -i '0,/enabled: true/{s/enabled: true/enabled: false/}' \
    "$1/crates/ctx-cli/src/config.rs"
  python3 - "$1/contracts/public-control-surface-v1.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
contract = json.loads(path.read_text())
analytics = next(
    control for control in contract["controls"]
    if control["config_key"] == "analytics.enabled"
)
analytics["released_default"] = {
    "value": False,
    "state": "off",
    "scope": "all_cli_installations",
}
analytics["previous_stable_default"] = {"value": False, "state": "off"}
path.write_text(json.dumps(contract, indent=2) + "\n")
PY
}

rewrite_pinned_history_to_hide_a_regression() {
  rewrite_history_to_hide_a_regression "$1"
  python3 - "$1/contracts/stable-defaults/v0.25.0.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
snapshot = json.loads(path.read_text())
snapshot["defaults"]["analytics.enabled"] = False
path.write_text(json.dumps(snapshot, indent=2) + "\n")
PY
}

add_undocumented_helper_env() {
  sed -i '/fn local_usage_env_override() -> LocalUsageEnvOverride {/a\
    let _undocumented = env::var_os("CTX_UNDOCUMENTED_HELPER_CONTROL");' \
    "$1/crates/ctx-cli/src/config.rs"
}

add_uncontained_retired_control() {
  python3 - "$1/scripts/uncontained.rs" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
control = "CTX_" + "FUNCTIONS_BASE"
path.write_text(f'const RETIRED: &str = "{control}";\n')
PY
}

expect_fail inventory-default \
  'analytics delivery released default differs from empty-config runtime' \
  change_inventory_default
expect_fail unapproved-change \
  'analytics delivery changed default lacks deliberate-change approval' \
  make_unapproved_change
expect_fail unscoped-evidence \
  'analytics delivery deliberate-change approval lacks scoped commit evidence' \
  add_unscoped_evidence
expect_fail runtime-default \
  'automatic upgrade mode released default differs from empty-config runtime' \
  change_runtime_default
expect_fail rewritten-history \
  'analytics delivery previous stable default differs from v0.25.0' \
  rewrite_history_to_hide_a_regression
expect_fail rewritten-pinned-history \
  'pinned previous stable snapshot digest differs for v0.25.0' \
  rewrite_pinned_history_to_hide_a_regression
expect_fail undocumented-helper-env \
  'config environment variables differ from contract' \
  add_undocumented_helper_env
expect_fail uncontained-retired-control \
  'retired controls remain' \
  add_uncontained_retired_control

printf 'public control surface checker tests passed\n'
