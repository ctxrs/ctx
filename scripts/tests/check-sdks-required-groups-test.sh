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
  local tool
  fixture="${test_root}/${name}"
  mkdir -p \
    "${fixture}/bin" \
    "${fixture}/scripts" \
    "${fixture}/contracts/agent-history-v2" \
    "${fixture}/crates/ctx-protocol" \
    "${fixture}/crates/ctx-sdk" \
    "${fixture}/sdks/dotnet/src/Ctx.AgentHistory" \
    "${fixture}/sdks/jvm/scripts" \
    "${fixture}/sdks/jvm/src/main/java" \
    "${fixture}/sdks/jvm/src/test/java" \
    "${fixture}/sdks/jvm/examples" \
    "${fixture}/sdks/python" \
    "${fixture}/sdks/typescript/examples" \
    "${fixture}/sdks/typescript/src" \
    "${fixture}/sdks/typescript/test" \
    "${fixture}/sdks/swift" \
    "${fixture}/sdks/dotnet/tests/Ctx.AgentHistory.Tests"
  cp "${repo_root}/scripts/check-sdks.sh" "${fixture}/scripts/check-sdks.sh"
  cp "${repo_root}/scripts/check-sdk-no-publish.sh" \
    "${fixture}/scripts/check-sdk-no-publish.sh"
  cp "${repo_root}/sdks/jvm/scripts/test" \
    "${fixture}/sdks/jvm/scripts/test"
  chmod 755 "${fixture}/sdks/jvm/scripts/test"
  # Keep missing-tool mutations independent of the host image. Expose only
  # the base utilities exercised by the fixture; SDK commands are supplied by
  # each case below.
  for tool in bash cp dirname find grep head mkdir mktemp rm sort; do
    ln -s "$(command -v "${tool}")" "${fixture}/bin/${tool}"
  done
  printf '{"private": true}\n' >"${fixture}/sdks/typescript/package.json"
  : >"${fixture}/sdks/typescript/package-lock.json"
  : >"${fixture}/sdks/typescript/tsconfig.types.json"
  printf '[tool.ctx]\npublish = false\n' >"${fixture}/sdks/python/pyproject.toml"
  printf '[package]\npublish = false\n' >"${fixture}/crates/ctx-sdk/Cargo.toml"
  printf '[package]\npublish = false\n' >"${fixture}/crates/ctx-protocol/Cargo.toml"
  : >"${fixture}/sdks/jvm/README.md"
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
    PATH="${fixture}/bin" \
    SDK_JVM_FAIL_STAGE="${SDK_JVM_FAIL_STAGE:-}" \
    SDK_SWIFT_VERSION_OUTPUT="${SDK_SWIFT_VERSION_OUTPUT:-}" \
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

expect_swift_version_success() {
  local name="$1"
  local version_output="$2"
  make_fixture "${name}"
  write_executable "${fixture}/bin/swift" \
    'if [[ "${1:-}" == "--version" ]]; then printf "%s\n" "${SDK_SWIFT_VERSION_OUTPUT}"; else printf "swift %s\n" "$*" >>"${SDK_TEST_LOG}"; fi'
  SDK_SWIFT_VERSION_OUTPUT="${version_output}" run_check \
    --groups=swift --required-groups=swift
  grep -Fq 'swift test --package-path sdks/swift --scratch-path ' "${log}" \
    || fail "accepted Swift version did not execute tests: ${name}"
}

expect_swift_version_failure() {
  local name="$1"
  local version_output="$2"
  make_fixture "${name}"
  write_executable "${fixture}/bin/swift" \
    'if [[ "${1:-}" == "--version" ]]; then printf "%s\n" "${SDK_SWIFT_VERSION_OUTPUT}"; else printf "swift %s\n" "$*" >>"${SDK_TEST_LOG}"; fi'
  if SDK_SWIFT_VERSION_OUTPUT="${version_output}" run_check \
    --groups=swift --required-groups=swift; then
    fail "unsupported Swift version unexpectedly succeeded: ${name}"
  fi
  grep -Fq 'required SDK group unavailable: swift (Swift 5.9+ required; found ' "${output}" \
    || fail "Swift version rejection did not fail closed: ${name}"
  if [[ -e "${log}" ]] && grep -Fq 'swift test ' "${log}"; then
    fail "rejected Swift version executed tests: ${name}"
  fi
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

make_fixture required-jvm-execution
write_executable "${fixture}/bin/javac" \
  'if [[ "${1:-}" == "-version" ]]; then printf "javac 17.0.1\n"; exit 0; fi' \
  'printf "javac %s\n" "$*" >>"${SDK_TEST_LOG}"' \
  'if [[ "${SDK_JVM_FAIL_STAGE:-}" == "compile" ]]; then printf "forced JVM compile failure\n" >&2; exit 41; fi'
write_executable "${fixture}/bin/java" \
  'printf "java %s\n" "$*" >>"${SDK_TEST_LOG}"' \
  'case "${SDK_JVM_FAIL_STAGE:-}:$*" in' \
  '  tests:*AgentHistoryClientTest*) printf "forced JVM tests failure\n" >&2; exit 42 ;;' \
  '  example:*ToyAgentHistoryApp*) printf "forced JVM example failure\n" >&2; exit 43 ;;' \
  'esac'
run_check --groups=jvm --required-groups=jvm
for completed_stage in compile tests example; do
  grep -Fq "JVM SDK stage complete: ${completed_stage}" "${output}" \
    || fail "JVM gate did not observe completed ${completed_stage} stage"
done
[[ "$(grep -c '^javac ' "${log}")" == "3" ]] \
  || fail 'JVM gate did not execute all three compilation stages'
[[ "$(grep -c '^java ' "${log}")" == "2" ]] \
  || fail 'JVM gate did not execute tests and example'
for failed_stage in compile tests example; do
  : >"${log}"
  if SDK_JVM_FAIL_STAGE="${failed_stage}" run_check \
    --groups=jvm --required-groups=jvm; then
    fail "JVM gate accepted forced ${failed_stage} failure"
  fi
  grep -Fq "forced JVM ${failed_stage} failure" "${output}" \
    || fail "JVM gate did not expose forced ${failed_stage} failure"
  if grep -Fq "JVM SDK stage complete: ${failed_stage}" "${output}"; then
    fail "JVM gate emitted completion for failed ${failed_stage} stage"
  fi
done

expect_swift_version_success \
  swift-observed-apple-5-10 \
  'swift-driver version: 1.90.11.1 Apple Swift version 5.10 (swiftlang-5.10.0.13 clang-1500.3.9.4)'
expect_swift_version_success \
  swift-upstream-5-9 \
  'Swift version 5.9 (swift-5.9-RELEASE)'
expect_swift_version_success \
  swift-apple-5-10-patch \
  'Apple Swift version 5.10.1 (swiftlang-5.10.1 clang-1500.3.9.4)'
expect_swift_version_success \
  swift-upstream-6-qualifier \
  'Swift version 6.2-dev (LLVM abcdef)'
expect_swift_version_failure \
  swift-apple-5-8 \
  'Apple Swift version 5.8.1 (swiftlang-5.8.1 clang-1403.0.22.11)'
expect_swift_version_failure \
  swift-malformed-version \
  'Apple Swift version 5.x (swiftlang-malformed)'
expect_swift_version_failure \
  swift-missing-label \
  'swift-driver version: 9.90.1'
expect_swift_version_failure \
  swift-misleading-driver-prefix \
  'swift-driver version: 9.90.1 Apple Swift version 5.8.1 (swiftlang-5.8.1 clang-1403.0.22.11)'

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
