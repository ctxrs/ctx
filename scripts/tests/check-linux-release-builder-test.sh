#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "${repo_root}"

checker="scripts/check-linux-release-builder.sh"
validator="scripts/validate-linux-release-builder.py"
public_entrypoint="scripts/build-public-cli-artifact.sh"
internal_entrypoint="scripts/build-linux-release-offline.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/ctx-linux-builder-contract.XXXXXX")"
trap 'rm -rf -- "${tmp}"' EXIT

python3 scripts/tests/validate-linux-release-builder-test.py "${validator}"

for fixed_path in \
  /etc/os-release \
  /usr/bin/getconf \
  /usr/bin/python3 \
  /opt/cargo/bin/rustc \
  /opt/cargo/bin/cargo; do
  grep -Fq "\"${fixed_path}\"" "${checker}"
done
grep -Fq -- '-I scripts/validate-linux-release-builder.py' "${checker}"

if grep -Fq 'CTX_TEST_LINUX_RELEASE_' "${checker}"; then
  echo "production builder checker still contains a test override" >&2
  exit 1
fi
if grep -Eq \
  'CTX_PUBLIC_CLI_(IN_CONTAINER|PHASE|PREPARED_DIR)' \
  "${public_entrypoint}" "${internal_entrypoint}"; then
  echo "Linux release entrypoint still contains a caller-selected internal phase" >&2
  exit 1
fi
if grep -Fq 'CTX_TEST_ONLY_ALLOW_EMULATED_LINUX_BUILD' "${public_entrypoint}"; then
  echo "production Linux entrypoint still contains an emulation override" >&2
  exit 1
fi
if grep -Fq \
  'bash scripts/build-public-cli-artifact.sh "${platform}"' \
  "${public_entrypoint}"; then
  echo "Linux release construction still recursively selects the public entrypoint" >&2
  exit 1
fi
grep -Fq \
  'bash scripts/build-linux-release-offline.sh "${platform}" "${target}"' \
  "${public_entrypoint}"
grep -Fq 'readonly prepared_dir="/prepared"' "${internal_entrypoint}"
grep -Fq 'readonly target_dir="/release-target"' "${internal_entrypoint}"
grep -Fq 'readonly artifact_dir="/artifacts"' "${internal_entrypoint}"
grep -Fq 'requires the fixed /work container mount' "${internal_entrypoint}"

expect_forbidden() {
  local entrypoint="$1"
  local variable="$2"
  local expected="forbidden Linux release environment variable: ${variable}"
  shift 2
  if env "${variable}=forged" "$@" \
    >"${tmp}/${entrypoint}-${variable}.out" \
    2>"${tmp}/${entrypoint}-${variable}.err"; then
    printf 'forged %s environment reached %s\n' \
      "${variable}" "${entrypoint}" >&2
    exit 1
  fi
  if [[ "${entrypoint}:${variable}" == \
    "public:CTX_TEST_ONLY_ALLOW_DIRTY_RELEASE_BUILD" ]]; then
    expected="forbidden public release environment variable: ${variable}"
  fi
  grep -Fq \
    "${expected}" \
    "${tmp}/${entrypoint}-${variable}.err"
}

for variable in \
  CTX_TEST_LINUX_RELEASE_BUILDER_CONTRACT \
  CTX_TEST_LINUX_RELEASE_OS_RELEASE \
  CTX_TEST_LINUX_RELEASE_ARBITRARY \
  CTX_TEST_LINUX_RELEASE-FORGED \
  CTX_TEST_ONLY_ALLOW_DIRTY_RELEASE_BUILD \
  CTX_TEST_ONLY_ALLOW_EMULATED_LINUX_BUILD \
  CTX_PUBLIC_CLI_IN_CONTAINER \
  CTX_PUBLIC_CLI_PHASE \
  CTX_PUBLIC_CLI_PREPARED_DIR; do
  expect_forbidden checker "${variable}" \
    "${checker}" x86_64-unknown-linux-gnu
  expect_forbidden public "${variable}" \
    "${public_entrypoint}" linux-x64
  expect_forbidden internal "${variable}" \
    "${internal_entrypoint}" linux-x64 x86_64-unknown-linux-gnu
done

printf 'Linux release builder contract tests passed\n'
