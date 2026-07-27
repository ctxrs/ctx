#!/usr/bin/env bash
set -euo pipefail

while IFS= read -r -d '' entry; do
  name="${entry%%=*}"
  case "${name}" in
    CTX_TEST_LINUX_RELEASE*|\
    CTX_TEST_ONLY_ALLOW_DIRTY_RELEASE_BUILD|\
    CTX_TEST_ONLY_ALLOW_EMULATED_LINUX_BUILD|\
    CTX_PUBLIC_CLI_IN_CONTAINER|\
    CTX_PUBLIC_CLI_PHASE|\
    CTX_PUBLIC_CLI_PREPARED_DIR)
      printf 'error: forbidden Linux release environment variable: %s\n' \
        "${name}" >&2
      exit 1
      ;;
  esac
done < <(/usr/bin/env -0)
