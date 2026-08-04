#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-sdk-required-groups-test.XXXXXX")"
trap 'rm -rf "${test_root}"' EXIT

fail() {
  printf 'check-sdks required-group test failed: %s\n' "$*" >&2
  exit 1
}

make_fixture() {
  local name="$1"
  fixture="${test_root}/${name}"
  mkdir -p \
    "${fixture}/bin" \
    "${fixture}/scripts" \
    "${fixture}/contracts/agent-history-v1" \
    "${fixture}/crates/ctx-protocol" \
    "${fixture}/crates/ctx-sdk" \
    "${fixture}/sdks/dotnet/src/Ctx.AgentHistory" \
    "${fixture}/sdks/python" \
    "${fixture}/sdks/typescript/examples" \
    "${fixture}/sdks/typescript/src" \
    "${fixture}/sdks/typescript/test" \
    "${fixture}/sdks/swift" \
    "${fixture}/sdks/dotnet/tests/Ctx.AgentHistory.Tests"
  cp "${repo_root}/scripts/check-sdks.sh" "${fixture}/scripts/check-sdks.sh"
  cp "${repo_root}/scripts/check-sdk-no-publish.sh" \
    "${fixture}/scripts/check-sdk-no-publish.sh"
  printf '{"private": true}\n' >"${fixture}/sdks/typescript/package.json"
  : >"${fixture}/sdks/typescript/package-lock.json"
  : >"${fixture}/sdks/typescript/tsconfig.types.json"
  printf '[tool.ctx]\npublish = false\n' >"${fixture}/sdks/python/pyproject.toml"
  printf '[package]\npublish = false\n' >"${fixture}/crates/ctx-sdk/Cargo.toml"
  printf '[package]\npublish = false\n' >"${fixture}/crates/ctx-protocol/Cargo.toml"
  printf '<IsPackable>false</IsPackable>\n' \
    >"${fixture}/sdks/dotnet/src/Ctx.AgentHistory/Ctx.AgentHistory.csproj"
  : >"${fixture}/sdks/swift/Package.swift"
  : >"${fixture}/sdks/dotnet/tests/Ctx.AgentHistory.Tests/Ctx.AgentHistory.Tests.csproj"
  # Keep harness output outside the fixture: the contracts group deliberately
  # scans every fixture file for forbidden publish commands.
  log="${test_root}/${name}.commands.log"
  output="${test_root}/${name}.output.log"
}

write_executable() {
  local path="$1"
  shift
  {
    printf '#!/usr/bin/env bash\n'
    printf '%s\n' "$@"
  } >"${path}"
  chmod 755 "${path}"
}

run_check() {
  env \
    PATH="${fixture}/bin:/usr/bin:/bin" \
    SDK_TEST_LOG="${log}" \
    bash "${fixture}/scripts/check-sdks.sh" "$@" >"${output}" 2>&1
}

expect_failure() {
  local expected="$1"
  shift
  if run_check "$@"; then
    fail "command unexpectedly succeeded: $*"
  fi
  grep -Fq -- "${expected}" "${output}" \
    || fail "failure output did not contain: ${expected}"
}

make_fixture required-missing
write_executable "${fixture}/bin/node" \
  'printf "v20.11.0\\n"'
expect_failure \
  'required SDK group unavailable: typescript (npm unavailable)' \
  --groups=typescript --required-groups=typescript

make_fixture required-old-version
write_executable "${fixture}/bin/node" \
  'printf "v18.20.0\\n"'
write_executable "${fixture}/bin/npm" \
  'printf "10.8.0\\n"'
expect_failure \
  'required SDK group unavailable: typescript (Node.js 20.0+ required; found v18.20.0)' \
  --groups=typescript --required-groups=typescript

make_fixture contracts-without-rg
write_executable "${fixture}/bin/python3" \
  'if [[ "${1:-}" == "--version" ]]; then printf "Python 3.12.4\\n"; fi'
write_executable "${fixture}/bin/rg" \
  'exit 127'
run_check --groups=contracts --required-groups=contracts
grep -Fq 'SDK groups complete: selected=contracts required=contracts skipped=0' "${output}" \
  || fail 'contracts group did not complete without ripgrep'
printf '#!/usr/bin/env bash\nnpm publish\n' >"${fixture}/release.sh"
expect_failure \
  'SDK publish guard failed: live SDK package-manager publish command found outside docs/policy text' \
  --groups=contracts --required-groups=contracts

make_fixture required-positive
write_executable "${fixture}/bin/node" \
  'printf "v20.11.0\\n"'
write_executable "${fixture}/bin/npm" \
  'printf "npm %s\\n" "$*" >>"${SDK_TEST_LOG}"' \
  'if [[ "${1:-}" == "--version" ]]; then printf "10.8.0\\n"; fi'
write_executable "${fixture}/bin/swift" \
  'if [[ "${1:-}" == "--version" ]]; then printf "Swift version 5.10.1\\n"; else printf "swift %s\\n" "$*" >>"${SDK_TEST_LOG}"; fi'
write_executable "${fixture}/bin/dotnet" \
  'if [[ "${1:-}" == "--version" ]]; then printf "8.0.303\\n"; else printf "dotnet %s\\n" "$*" >>"${SDK_TEST_LOG}"; fi'
run_check \
  --groups=typescript,swift,dotnet \
  --required-groups=typescript,swift,dotnet
grep -Fq 'SDK groups complete: selected=typescript,swift,dotnet required=typescript,swift,dotnet skipped=0' "${output}" \
  || fail 'positive run did not report all required groups complete'
grep -Fxq 'npm ci --prefix sdks/typescript --ignore-scripts' "${log}" \
  || fail 'positive run did not install locked TypeScript dependencies'
grep -Fxq 'npm test --prefix sdks/typescript' "${log}" \
  || fail 'positive run did not execute TypeScript tests'
grep -Fq 'swift test --package-path sdks/swift --scratch-path ' "${log}" \
  || fail 'positive run did not execute Swift tests'
grep -Fxq 'dotnet build sdks/dotnet/tests/Ctx.AgentHistory.Tests/Ctx.AgentHistory.Tests.csproj --configuration Release --nologo' "${log}" \
  || fail 'positive run did not compile the .NET test project'
grep -Fxq 'dotnet run --project sdks/dotnet/tests/Ctx.AgentHistory.Tests/Ctx.AgentHistory.Tests.csproj --configuration Release --no-build' "${log}" \
  || fail 'positive run did not execute the compiled .NET tests'

make_fixture optional-missing
run_check --groups=swift
grep -Fq 'skip: swift SDK group (swift unavailable)' "${output}" \
  || fail 'optional local group did not retain skip convenience'

make_fixture invalid-selection
expect_failure \
  'required SDK group is not selected: dotnet' \
  --groups=swift --required-groups=dotnet
expect_failure \
  'unknown SDK group: typo' \
  --groups=typo

printf 'check-sdks required-group tests ok\n'
