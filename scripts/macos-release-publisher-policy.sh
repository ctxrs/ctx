#!/usr/bin/env bash

# SHA-256 of the allowed Apple Team ID. Keep the identity itself out of source
# while retaining a project-specific trust anchor across certificate rotation.
readonly CTX_MACOS_RELEASE_TEAM_ID_SHA256="913603530eb11be6c4e501c7a8190bee4192f3536ac195add60716e3e372594a"

ctx_macos_release_team_id_matches_policy() {
  local team_id="$1"
  local expected="${CTX_MACOS_RELEASE_TEAM_ID_SHA256}"
  local actual

  if [[ "${CTX_LOCAL_MACOS_SIGNING_LIVE_TEST:-0}" == "1" \
    && "${CTX_TEST_ONLY_MACOS_HOST:-}" == "Darwin" \
    && -z "${BUILDKITE:-}" \
    && -z "${CI:-}" \
    && -z "${GITHUB_ACTIONS:-}" ]]; then
    expected="${CTX_TEST_ONLY_MACOS_RELEASE_TEAM_ID_SHA256:-${expected}}"
  fi
  [[ "${expected}" =~ ^[0-9a-f]{64}$ ]] || return 1
  actual="$(printf '%s' "${team_id}" | openssl dgst -sha256 -r 2>/dev/null | awk '{print $1}')"
  [[ "${actual}" == "${expected}" ]]
}
