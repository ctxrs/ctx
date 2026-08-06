#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--version" ]]; then
  printf 'bazel %s\n' "${CTX_FAKE_BAZEL_VERSION:-7.7.1}"
  exit 0
fi

: "${CTX_FAKE_BAZEL_LOG:?CTX_FAKE_BAZEL_LOG is required}"

command_name=""
mode=""
workspace=""
output=""
for argument in "$@"; do
  printf 'arg=%s\n' "${argument}" >>"${CTX_FAKE_BAZEL_LOG}"
  if [[ -z "${command_name}" && "${argument}" != --* ]]; then
    command_name="${argument}"
  fi
  case "${argument}" in
    generate-hashes|get-impacted-targets)
      mode="${argument}"
      ;;
    --workspacePath=*)
      workspace="${argument#*=}"
      ;;
    --output=*)
      output="${argument#*=}"
      ;;
  esac
done
printf 'env=RUST_TEST_THREADS=%s\n' "${RUST_TEST_THREADS:-}" >>"${CTX_FAKE_BAZEL_LOG}"

if [[ -n "${CTX_FAKE_BAZEL_FAIL_MODE:-}" && "${CTX_FAKE_BAZEL_FAIL_MODE}" == "${mode}" ]]; then
  exit 23
fi

case "${mode}" in
  generate-hashes)
    output="${!#}"
    if [[ -n "${CTX_FAKE_BAZEL_DELAY:-}" ]]; then
      sleep "${CTX_FAKE_BAZEL_DELAY}"
    fi
    printf '{"workspace":"%s"}\n' "${workspace}" >"${output}"
    printf 'event=generate-hashes workspace=%s output=%s\n' "${workspace}" "${output}" >>"${CTX_FAKE_BAZEL_LOG}"
    ;;
  get-impacted-targets)
    : "${CTX_FAKE_BAZEL_IMPACTED_FILE:?CTX_FAKE_BAZEL_IMPACTED_FILE is required}"
    cp "${CTX_FAKE_BAZEL_IMPACTED_FILE}" "${output}"
    printf 'event=get-impacted-targets output=%s\n' "${output}" >>"${CTX_FAKE_BAZEL_LOG}"
    ;;
  *)
    if [[ "${command_name}" == "query" && -n "${CTX_FAKE_BAZEL_QUERY_FILE:-}" ]]; then
      cat "${CTX_FAKE_BAZEL_QUERY_FILE}"
    fi
    ;;
esac
