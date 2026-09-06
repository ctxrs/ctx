#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
public_ci_script="${repo_root}/scripts/buildkite-public-ci.sh"
test_root="$(mktemp -d "${TEST_TMPDIR:-${TMPDIR:-/tmp}}/ctx-buildkite-cache-test.XXXXXXXX")"
trap 'rm -rf -- "${test_root}"' EXIT

fail() {
  printf 'Buildkite public CI cache test failed: %s\n' "$*" >&2
  exit 1
}

checkout="${test_root}/builds/host/ctx-public-ci"
build_path="${test_root}/builds"
mkdir -p "${checkout}/scripts" "${build_path}"

make_probe() {
  local source="$1"
  local output="$2"
  awk '
    /^init_buildkite_job_tool_env$/ {
      print
      print "printf \047repository_cache=%s\\n\047 \"${CTX_BAZEL_REPOSITORY_CACHE}\""
      print "printf \047repository_contents=%s\\n\047 \"$(cd \"${CTX_BAZEL_REPOSITORY_CACHE}/contents\" && pwd -P)\""
      exit
    }
    { print }
  ' "${source}" > "${output}"
  chmod 0755 "${output}"
}

probe_env=(
  "PATH=/usr/bin:/bin"
  "BUILDKITE=true"
  "BUILDKITE_BUILD_ID=build-77"
  "BUILDKITE_BUILD_PATH=${build_path}"
  "BUILDKITE_BUILD_CHECKOUT_PATH=${checkout}"
  "BUILDKITE_JOB_ID=job:77/smoke"
  "TMPDIR=${test_root}/tmp"
)

run_probe() {
  env -i "${probe_env[@]}" "$@" \
    bash "${checkout}/scripts/buildkite-public-ci.sh" "${probe_args[@]}"
}

make_probe "${public_ci_script}" "${checkout}/scripts/buildkite-public-ci.sh"
probe_args=()
output="$(run_probe)"
expected_cache="${build_path}/ctx-public-ci-cache/job_77_smoke/bazel-repository"
expected_contents="${expected_cache}/contents"
grep -Fqx "repository_cache=${expected_cache}" <<<"${output}" \
  || fail "representative Buildkite environment resolved the wrong repository cache"
grep -Fqx "repository_contents=${expected_contents}" <<<"${output}" \
  || fail "representative Buildkite environment resolved contents inside the wrong root"
case "${expected_contents}/" in
  "${checkout}/"*) fail "representative contents cache is inside the checkout" ;;
esac

unsafe_cache="${checkout}/.buildkite-cache/bazel-repository"
if run_probe "CTX_BAZEL_REPOSITORY_CACHE=${unsafe_cache}" \
  >"${test_root}/unsafe.out" 2>"${test_root}/unsafe.err"; then
  fail "checkout-relative repository cache override was accepted"
fi
grep -Fq 'repository contents cache must be outside checkout' "${test_root}/unsafe.err" \
  || fail "checkout-relative rejection did not explain the cache boundary"

mkdir -p "${checkout}/linked-cache-target"
ln -s "${checkout}/linked-cache-target" "${test_root}/linked-cache"
if run_probe "CTX_BAZEL_REPOSITORY_CACHE=${test_root}/linked-cache" \
  >"${test_root}/linked.out" 2>"${test_root}/linked.err"; then
  fail "repository cache symlink resolving inside the checkout was accepted"
fi
grep -Fq 'repository contents cache must be outside checkout' "${test_root}/linked.err" \
  || fail "resolved-path rejection did not explain the cache boundary"

python3 - "${public_ci_script}" "${test_root}/mutated-buildkite-public-ci.sh" <<'PY'
import pathlib
import sys

source_path = pathlib.Path(sys.argv[1])
output_path = pathlib.Path(sys.argv[2])
source = source_path.read_text(encoding="utf-8")
safe = "${build_path%/}/ctx-public-ci-cache/${job_slug}/bazel-repository"
unsafe = "${repo_root}/.buildkite-cache/bazel-repository"
if source.count(safe) != 1:
    raise SystemExit("expected exactly one authoritative external repository-cache default")
output_path.write_text(source.replace(safe, unsafe, 1), encoding="utf-8")
PY
make_probe "${test_root}/mutated-buildkite-public-ci.sh" \
  "${checkout}/scripts/buildkite-public-ci.sh"
if run_probe >"${test_root}/mutation.out" 2>"${test_root}/mutation.err"; then
  fail "old checkout-relative default mutation was accepted"
fi
grep -Fq 'repository contents cache must be outside checkout' "${test_root}/mutation.err" \
  || fail "old-layout mutation did not exercise the cache-boundary rejection"

runner_task_root="${test_root}/runner-task"
release_checkout="${runner_task_root}/builds/host/ctx-public-release"
release_build_path="${runner_task_root}/builds"
mkdir -p "${release_checkout}/scripts" "${release_build_path}"
make_probe "${public_ci_script}" "${release_checkout}/scripts/buildkite-public-ci.sh"
checkout="${release_checkout}"
build_path="${release_build_path}"
probe_env=(
  "PATH=/usr/bin:/bin"
  "BUILDKITE=true"
  "BUILDKITE_BUILD_ID=build-88"
  "BUILDKITE_BUILD_PATH=${release_build_path}"
  "BUILDKITE_BUILD_CHECKOUT_PATH=${release_checkout}"
  "BUILDKITE_JOB_ID=job:88/release"
  "CTX_RUNNER_TASK_ROOT=${runner_task_root}"
  "TMPDIR=/tmp"
)
probe_args=(--mode=release)
output="$(run_probe)"
expected_release_tmp="${runner_task_root}/tmp/ctx-public-ci-job_88_release/bazel-test-tmp"
grep -Fqx "Buildkite release Bazel test TMPDIR: ${expected_release_tmp}" <<<"${output}" \
  || fail 'release mode did not bind Bazel tests to the task-local temporary root'

if run_probe "CTX_PUBLIC_CI_TEST_TMPDIR=${test_root}/outside-task-root" \
  >"${test_root}/outside.out" 2>"${test_root}/outside.err"; then
  fail 'release test TMPDIR outside the task-local bind was accepted'
fi
grep -Fq 'release test temporary path escaped task-local bind' "${test_root}/outside.err" \
  || fail 'outside release test TMPDIR did not explain the task-bind boundary'

printf '[workspace.package]\nversion = "1.3.2"\n' > "${checkout}/Cargo.toml"
for mode_spelling in joined separate; do
  if [[ "${mode_spelling}" == joined ]]; then
    probe_args=(--mode=release)
  else
    probe_args=(--mode release)
  fi
  output="$(run_probe BUILDKITE_BRANCH=release/1.3.2)"
  grep -Fqx 'Buildkite release branch: release/1.3.2, source version: 1.3.2' <<<"${output}" \
    || fail 'fixed release branch did not check the source version'
  grep -Fqx "Buildkite release Bazel test TMPDIR: ${expected_release_tmp}" <<<"${output}" \
    || fail 'fixed release branch lost the task-local temporary root'
done

probe_args=(--mode=release)
case_number=0
for manifest in \
  $'[workspace.package]\nversion = "1.3.3"' \
  $'[workspace.package]\nversion = "1.3.2-rc.1"' \
  $'[workspace.package]\nversion = "1.3.2 "' \
  $'[workspace.package]\nversion = 132' \
  $'[workspace.package]\nname = "ctx"' \
  $'[workspace]\npackage = "1.3.2"' \
  $'[workspace.package]\nversion = "1.3.2"\nversion = "1.3.3"' \
  $'[workspace.package]\nversion = "1.3.2' \
  missing-file; do
  case_number=$((case_number + 1))
  printf '%s\n' "${manifest}" > "${checkout}/Cargo.toml"
  if [[ "${manifest}" == missing-file ]]; then rm -- "${checkout}/Cargo.toml"; fi
  uncreated_tool_root="${runner_task_root}/rejected-version-${case_number}"
  status=0
  run_probe BUILDKITE_BRANCH=release/1.3.2 CTX_RELEASE_VERSION=1.3.2 \
    "CTX_PUBLIC_CI_TOOL_ROOT=${uncreated_tool_root}" \
    >"${test_root}/version.out" 2>"${test_root}/version.err" || status=$?
  [[ "${status}" == 1 ]] || fail 'invalid release source version did not exit one'
  grep -Fqx 'Buildkite release/1.3.2 requires checked-out workspace version 1.3.2' \
    "${test_root}/version.err" || fail 'source version rejection did not explain the boundary'
  [[ ! -e "${uncreated_tool_root}" && ! -s "${test_root}/version.out" ]] \
    || fail 'source version rejection occurred after tool setup'
done

printf '[workspace.package]\nversion = "1.3.3"\n' > "${checkout}/Cargo.toml"
output="$(run_probe BUILDKITE_BRANCH=main)"
grep -Fqx "Buildkite release Bazel test TMPDIR: ${expected_release_tmp}" <<<"${output}" \
  || fail 'main release behavior changed'
for mode in ci nightly; do
  probe_args=(--mode="${mode}")
  output="$(run_probe BUILDKITE_BRANCH=release/1.3.2)"
  grep -Fq 'repository_cache=' <<<"${output}" \
    || fail 'ordinary CI or nightly acquired the release-only version restriction'
done

printf 'Buildkite public CI cache test ok: repository contents resolve outside checkout\n'
