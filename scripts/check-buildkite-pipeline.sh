#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/ci-common.sh
source "${script_dir}/ci-common.sh"

contract_failures=0
contract_summary=""

note() {
  printf '%s\n' "$*" | tee -a "${contract_summary}"
}

fail_contract() {
  contract_failures=$(( contract_failures + 1 ))
  note "FAIL: $*"
}

pass_contract() {
  note "PASS: $*"
}

file_contains() {
  local file="$1"
  local text="$2"

  if command -v rg >/dev/null 2>&1; then
    rg --fixed-strings -q "${text}" "${file}"
    return $?
  fi

  grep -F -q "${text}" "${file}"
}

require_text() {
  local description="$1"
  local file="$2"
  local text="$3"

  if file_contains "${file}" "${text}"; then
    pass_contract "${description}"
  else
    fail_contract "${description} (${file} missing ${text})"
  fi
}

validate_yaml_if_possible() {
  local pipeline="$1"

  if command -v buildkite-agent >/dev/null 2>&1; then
    if BUILDKITE_AGENT_ACCESS_TOKEN="${BUILDKITE_AGENT_ACCESS_TOKEN:-dry-run}" \
      buildkite-agent pipeline upload \
        --dry-run \
        --format json \
        --no-interpolation \
        --no-color \
        --log-level error \
        "${pipeline}" > "${CTX_ARTIFACT_DIR}/pipeline-dry-run.json"; then
      pass_contract "Buildkite agent dry-run parser accepts pipeline"
    else
      fail_contract "Buildkite agent dry-run parser"
    fi
    return 0
  fi

  if ! command -v ruby >/dev/null 2>&1; then
    note "SKIP: YAML parser check (buildkite-agent and ruby unavailable)"
    return 0
  fi

  if ruby -e '
    require "yaml"
    data = YAML.load_file(ARGV.fetch(0))
    unless data.is_a?(Hash) && data["steps"].is_a?(Array)
      abort "pipeline must be a mapping with a steps array"
    end
    keys = data["steps"].map { |step| step.is_a?(Hash) ? step["key"] : nil }.compact
    duplicates = keys.group_by(&:itself).select { |_key, values| values.size > 1 }.keys
    abort "duplicate step keys: #{duplicates.join(", ")}" unless duplicates.empty?
  ' "${pipeline}"; then
    pass_contract "Buildkite YAML parses and step keys are unique"
  else
    fail_contract "Buildkite YAML parse or duplicate-key check"
  fi
}

validate_contract() {
  local pipeline=".buildkite/pipeline.yml"
  local release_script="scripts/release-dry-run.sh"
  local windows_script="scripts/ci-windows.ps1"
  local blocker_script="scripts/release-platform-blocker.sh"

  test -f "${pipeline}" || fail_contract "pipeline file exists"
  test -f "${release_script}" || fail_contract "release dry-run script exists"
  test -f "${windows_script}" || fail_contract "Windows PowerShell CI script exists"
  if [[ -f scripts/ci-windows-bash.cmd ]]; then
    fail_contract "Windows Bash wrapper has been removed"
  fi
  test -f "${blocker_script}" || fail_contract "release platform blocker script exists"

  validate_yaml_if_possible "${pipeline}"

  require_text "pipeline contract step" "${pipeline}" 'key: "pipeline-contract"'
  require_text "Linux fmt step" "${pipeline}" 'key: "fmt"'
  require_text "Linux docs step" "${pipeline}" 'key: "docs"'
  require_text "Linux cargo check step" "${pipeline}" 'key: "cargo-check"'
  require_text "Linux clippy step" "${pipeline}" 'key: "clippy"'
  require_text "Linux cargo test step" "${pipeline}" 'key: "cargo-test"'
  require_text "Linux examples step" "${pipeline}" 'key: "examples"'
  require_text "Linux Bazel step" "${pipeline}" 'key: "bazel"'
  require_text "Linux x64 smoke step" "${pipeline}" 'key: "platform-smoke-linux-x64"'
  require_text "macOS arm64 smoke step" "${pipeline}" 'key: "platform-smoke-macos-arm64"'
  require_text "macOS x64 smoke step" "${pipeline}" 'key: "platform-smoke-macos-x64"'
  require_text "Windows x64 smoke step" "${pipeline}" 'key: "platform-smoke-windows-x64"'
  require_text "Linux release dry-run step" "${pipeline}" 'key: "release-dry-run-linux-x64"'
  require_text "macOS arm64 release dry-run step" "${pipeline}" 'key: "release-dry-run-macos-arm64"'
  require_text "macOS x64 release dry-run step" "${pipeline}" 'key: "release-dry-run-macos-x64"'
  require_text "Windows x64 release dry-run step" "${pipeline}" 'key: "release-dry-run-windows-x64"'
  require_text "FreeBSD documented blocker step" "${pipeline}" 'key: "freebsd-x64-blocker"'

  require_text "Linux verification queue" "${pipeline}" 'queue: "release-linux-managed"'
  require_text "Linux verification runner class" "${pipeline}" 'ctx-runner-class: "release-linux-x64-stage"'
  require_text "Linux release queue" "${pipeline}" 'queue: "release-linux-managed"'
  require_text "Linux release runner class" "${pipeline}" 'ctx-runner-class: "release-linux-x64-stage"'
  require_text "macOS arm64 queue" "${pipeline}" 'queue: "ctx-mac-gui-shared-arm64"'
  require_text "macOS x64 queue" "${pipeline}" 'queue: "ctx-mac-gui-shared-x64"'
  require_text "Windows x64 queue" "${pipeline}" 'queue: "windows-x64"'
  require_text "Windows PowerShell wrapper" "${pipeline}" 'powershell -NoProfile -ExecutionPolicy Bypass -File scripts\\ci-windows.ps1'
  require_text "macOS custom checkout plugin" "${pipeline}" 'custom-checkout#v1.8.0'
  require_text "macOS arm64 smoke isolated checkout" "${pipeline}" 'ctx-macos-arm64-smoke-${BUILDKITE_BUILD_NUMBER}'
  require_text "macOS x64 smoke isolated checkout" "${pipeline}" 'ctx-macos-x64-smoke-${BUILDKITE_BUILD_NUMBER}'
  require_text "macOS arm64 release isolated checkout" "${pipeline}" 'ctx-macos-arm64-release-${BUILDKITE_BUILD_NUMBER}'
  require_text "macOS x64 release isolated checkout" "${pipeline}" 'ctx-macos-x64-release-${BUILDKITE_BUILD_NUMBER}'

  require_text "docs command wired" "${pipeline}" './scripts/check.sh docs'
  require_text "examples command wired" "${pipeline}" './scripts/check.sh examples'
  require_text "Bazel is required in CI" "${pipeline}" 'CTX_REQUIRE_BAZEL=1 ./scripts/check.sh bazel'
  require_text "platform smoke command wired" "${pipeline}" './scripts/check.sh platform-smoke'
  require_text "Windows platform smoke command wired" "${pipeline}" 'scripts\\ci-windows.ps1 platform-smoke'
  require_text "Windows release dry-run command wired" "${pipeline}" 'scripts\\ci-windows.ps1 release-dry-run'
  require_text "Linux host triple guard" "${pipeline}" 'CTX_EXPECT_HOST_TRIPLE: "x86_64-unknown-linux-gnu"'
  require_text "macOS arm64 host triple guard" "${pipeline}" 'CTX_EXPECT_HOST_TRIPLE: "aarch64-apple-darwin"'
  require_text "macOS x64 host triple guard" "${pipeline}" 'CTX_EXPECT_HOST_TRIPLE: "x86_64-apple-darwin"'
  require_text "Windows x64 host triple guard" "${pipeline}" 'CTX_EXPECT_HOST_TRIPLE: "x86_64-pc-windows-msvc"'
  require_text "FreeBSD target recorded" "${pipeline}" 'x86_64-unknown-freebsd'

  require_text "release script is dry-run only" "${release_script}" '"dry_run": true'
  require_text "release script does not publish" "${release_script}" '"upload": false'
  require_text "release script enforces host triple" "${release_script}" 'ctx_require_host_triple "${CTX_EXPECT_HOST_TRIPLE:-}"'
  require_text "Windows script bootstraps Rust" "${windows_script}" 'Ensure-Rust-Toolchain'
  require_text "Windows script uses target tool cache" "${windows_script}" 'target\tool-cache\cargo'
  require_text "Windows script supports platform smoke" "${windows_script}" 'platform-smoke'
  require_text "Windows script supports release dry-run" "${windows_script}" 'release-dry-run'
  require_text "Windows script uses named Cargo args for platform smoke" "${windows_script}" 'Run-Cargo -Args (@("build", "-p", "ctx", "--bin", "ctx") + $locked)'
  require_text "Windows script uses named Cargo args for release dry-run" "${windows_script}" 'Run-Cargo -Args (@("build", "--workspace", "--release", "--bins") + $locked)'
  require_text "FreeBSD blocker marks publishing false" "${blocker_script}" '"publishing": false'

  if (( contract_failures > 0 )); then
    note "Buildkite pipeline contract failed with ${contract_failures} issue(s)."
    return 1
  fi

  note "Buildkite pipeline contract ok."
}

cd "${CTX_REPO_ROOT}"
CTX_ARTIFACT_DIR="${CTX_ARTIFACT_DIR:-target/ctx-artifacts/buildkite-contract}"
mkdir -p "${CTX_ARTIFACT_DIR}"
contract_summary="${CTX_ARTIFACT_DIR}/pipeline-contract.txt"
: > "${contract_summary}"
ctx_timing_init
trap ctx_timing_finish EXIT
ctx_run_timed "buildkite-pipeline-contract" validate_contract
