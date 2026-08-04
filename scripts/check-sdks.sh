#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

all_groups="contracts,typescript,python,go,jvm,swift,dotnet"
selected_groups="${CTX_SDK_GROUPS:-${all_groups}}"
required_groups="${CTX_SDK_REQUIRED_GROUPS:-}"
strict_toolchains="${CTX_SDK_STRICT_TOOLCHAINS:-0}"

usage() {
  cat <<'USAGE'
usage: scripts/check-sdks.sh [--groups GROUPS] [--required-groups GROUPS]

Groups: contracts, typescript, python, go, jvm, swift, dotnet, or all.
Groups are optional by default so a developer can run this command with only
locally installed toolchains. A required group fails closed when its SDK,
toolchain, or minimum supported toolchain version is unavailable.

Environment equivalents:
  CTX_SDK_GROUPS
  CTX_SDK_REQUIRED_GROUPS
  CTX_SDK_STRICT_TOOLCHAINS=1  Require every selected group.
USAGE
}

while (( "$#" > 0 )); do
  case "$1" in
    --groups=*) selected_groups="${1#--groups=}"; shift ;;
    --groups)
      shift
      (( "$#" > 0 )) || { printf 'missing value for --groups\n' >&2; exit 2; }
      selected_groups="$1"
      shift
      ;;
    --required-groups=*) required_groups="${1#--required-groups=}"; shift ;;
    --required-groups)
      shift
      (( "$#" > 0 )) || { printf 'missing value for --required-groups\n' >&2; exit 2; }
      required_groups="$1"
      shift
      ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
done

contains_group() {
  local list="$1"
  local group="$2"
  [[ ",${list}," == *",${group},"* ]]
}

normalize_groups() {
  local value="$1"
  local label="$2"
  local normalized=""
  local group
  local entries=()

  if [[ -z "${value}" ]]; then
    normalized_groups=""
    return 0
  fi
  if [[ "${value}" == "all" ]]; then
    normalized_groups="${all_groups}"
    return 0
  fi

  IFS=',' read -r -a entries <<<"${value}"
  for group in "${entries[@]}"; do
    case "${group}" in
      contracts|typescript|python|go|jvm|swift|dotnet) ;;
      '') printf '%s contains an empty SDK group\n' "${label}" >&2; exit 2 ;;
      *) printf '%s contains unknown SDK group: %s\n' "${label}" "${group}" >&2; exit 2 ;;
    esac
    if ! contains_group "${normalized}" "${group}"; then
      normalized="${normalized:+${normalized},}${group}"
    fi
  done
  normalized_groups="${normalized}"
}

normalize_groups "${selected_groups}" '--groups'
selected_groups="${normalized_groups}"
normalize_groups "${required_groups}" '--required-groups'
required_groups="${normalized_groups}"

case "${strict_toolchains}" in
  0) ;;
  1) required_groups="${selected_groups}" ;;
  *) printf 'CTX_SDK_STRICT_TOOLCHAINS must be 0 or 1\n' >&2; exit 2 ;;
esac

IFS=',' read -r -a required_entries <<<"${required_groups}"
for required_group in "${required_entries[@]}"; do
  [[ -z "${required_group}" ]] && continue
  if ! contains_group "${selected_groups}" "${required_group}"; then
    printf 'required SDK group is not selected: %s\n' "${required_group}" >&2
    exit 2
  fi
done

run() {
  printf '\n==> %s\n' "$*"
  "$@"
}

run_in_dir() {
  local dir="$1"
  shift
  printf '\n==> (cd %s && %s)\n' "$dir" "$*"
  (
    cd "$dir"
    "$@"
  )
}

skipped=0
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-sdk-check.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

group_required() {
  contains_group "${required_groups}" "$1"
}

unavailable() {
  local group="$1"
  shift
  if group_required "${group}"; then
    printf '\nrequired SDK group unavailable: %s (%s)\n' "${group}" "$*" >&2
    exit 1
  fi
  printf '\n==> skip: %s SDK group (%s)\n' "${group}" "$*"
  skipped=$((skipped + 1))
}

version_meets() {
  local output="$1"
  local minimum_major="$2"
  local minimum_minor="$3"
  if [[ ! "${output}" =~ ([0-9]+)\.([0-9]+) ]]; then
    return 1
  fi
  local actual_major="${BASH_REMATCH[1]}"
  local actual_minor="${BASH_REMATCH[2]}"
  (( actual_major > minimum_major \
    || (actual_major == minimum_major && actual_minor >= minimum_minor) ))
}

check_version() {
  local group="$1"
  local label="$2"
  local minimum="$3"
  local output="$4"
  local minimum_major="${minimum%%.*}"
  local minimum_minor="${minimum#*.}"
  if version_meets "${output}" "${minimum_major}" "${minimum_minor}"; then
    return 0
  fi
  unavailable "${group}" "${label} ${minimum}+ required; found ${output}"
  return 1
}

if contains_group "${selected_groups}" contracts; then
  if ! command -v python3 >/dev/null 2>&1; then
    unavailable contracts 'python3 unavailable'
  elif check_version contracts Python 3.10 "$(python3 --version 2>&1)"; then
    run python3 scripts/check-agent-history-contract.py
    run bash scripts/check-sdk-no-publish.sh
  fi
fi

if contains_group "${selected_groups}" typescript; then
  if [[ ! -f sdks/typescript/package.json ]]; then
    unavailable typescript 'sdks/typescript/package.json absent'
  elif ! command -v node >/dev/null 2>&1; then
    unavailable typescript 'node unavailable'
  elif ! command -v npm >/dev/null 2>&1; then
    unavailable typescript 'npm unavailable'
  elif check_version typescript Node.js 20.0 "$(node --version 2>&1)"; then
    typescript_root="${tmp_dir}/typescript-repo"
    mkdir -p \
      "${typescript_root}/contracts" \
      "${typescript_root}/sdks/typescript"
    cp \
      sdks/typescript/package.json \
      sdks/typescript/tsconfig.types.json \
      "${typescript_root}/sdks/typescript/"
    if [[ -f sdks/typescript/package-lock.json ]]; then
      cp sdks/typescript/package-lock.json \
        "${typescript_root}/sdks/typescript/"
    fi
    cp -R \
      sdks/typescript/examples \
      sdks/typescript/src \
      sdks/typescript/test \
      "${typescript_root}/sdks/typescript/"
    cp -R \
      contracts/agent-history-v1 \
      "${typescript_root}/contracts/"
    run npm --version
    if [[ -f sdks/typescript/package-lock.json ]]; then
      run_in_dir "${typescript_root}" \
        npm ci --prefix sdks/typescript --ignore-scripts
    fi
    run_in_dir "${typescript_root}" npm test --prefix sdks/typescript
  fi
fi

if contains_group "${selected_groups}" python; then
  if [[ ! -d sdks/python/tests ]]; then
    unavailable python 'sdks/python/tests absent'
  elif ! command -v python3 >/dev/null 2>&1; then
    unavailable python 'python3 unavailable'
  elif check_version python Python 3.10 "$(python3 --version 2>&1)"; then
    run python3 -m unittest discover -s sdks/python/tests
  fi
fi

if contains_group "${selected_groups}" go; then
  if [[ ! -f sdks/go/go.mod ]]; then
    unavailable go 'sdks/go/go.mod absent'
  elif [[ -n "${TEST_SRCDIR:-}" ]]; then
    unavailable go 'nested Bazel unavailable; Go is a sibling target in the owning suite'
  else
    run scripts/bazelw test \
      //sdks/go:go_sdk_tests \
      //sdks/go/examples/dogfood:dogfood_tests \
      --config=test
  fi
fi

if contains_group "${selected_groups}" jvm; then
  jvm_test=''
  if [[ ! -f sdks/jvm/README.md ]]; then
    unavailable jvm 'sdks/jvm absent'
  elif ! command -v javac >/dev/null 2>&1; then
    unavailable jvm 'javac unavailable'
  elif [[ -x sdks/jvm/scripts/test ]]; then
    jvm_test='sdks/jvm/scripts/test'
  elif [[ -x sdks/jvm/scripts/test.sh ]]; then
    jvm_test='sdks/jvm/scripts/test.sh'
  else
    unavailable jvm 'no executable sdks/jvm/scripts/test'
  fi
  if [[ -n "${jvm_test}" ]] \
    && check_version jvm Java 11.0 "$(javac -version 2>&1)"; then
    run "${jvm_test}"
  fi
fi

if contains_group "${selected_groups}" swift; then
  if [[ ! -f sdks/swift/Package.swift ]]; then
    unavailable swift 'sdks/swift/Package.swift absent'
  elif ! command -v swift >/dev/null 2>&1; then
    unavailable swift 'swift unavailable'
  elif check_version swift Swift 5.9 "$(swift --version 2>&1 | head -n 1)"; then
    run swift test --package-path sdks/swift --scratch-path "$tmp_dir/swift-build"
  fi
fi

if contains_group "${selected_groups}" dotnet; then
  dotnet_tests='sdks/dotnet/tests/Ctx.AgentHistory.Tests/Ctx.AgentHistory.Tests.csproj'
  if [[ ! -f "${dotnet_tests}" ]]; then
    unavailable dotnet "${dotnet_tests} absent"
  elif ! command -v dotnet >/dev/null 2>&1; then
    unavailable dotnet 'dotnet unavailable'
  elif check_version dotnet .NET 8.0 "$(dotnet --version 2>&1)"; then
    run dotnet build "${dotnet_tests}" --configuration Release --nologo
    run dotnet run --project "${dotnet_tests}" --configuration Release --no-build
  fi
fi

if [[ "${CTX_SDK_RUN_LOCAL_SMOKE:-0}" == "1" ]]; then
  run bash scripts/sdk-local-smoke.sh
fi

printf '\nSDK groups complete: selected=%s required=%s skipped=%s\n' \
  "${selected_groups}" "${required_groups:-none}" "${skipped}"
