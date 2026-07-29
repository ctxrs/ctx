#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
smoke="${repo_root}/scripts/run-native-candidate-smoke.sh"
pro_status_fixtures="${repo_root}/scripts/tests/fixtures/native-candidate-pro-status"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/ctx-native-smoke-test.XXXXXX")"
trap 'rm -rf "${tmp}"' EXIT

fake="${tmp}/ctx"
cat > "${fake}" <<'EOF'
#!/bin/sh
set -eu

test "${CTX_DAEMON_AUTOSTART_OFF:-}" = 1
test -n "${CTX_DATA_ROOT:-}"
test -n "${HOME:-}"
test -n "${XDG_CONFIG_HOME:-}"
test -n "${XDG_CACHE_HOME:-}"
test "${HOME}" != "${ORIGINAL_HOME:-not-in-clean-env}"
if data_root_mode="$(stat -c '%a' "${CTX_DATA_ROOT}" 2>/dev/null)"; then
  :
else
  data_root_mode="$(stat -f '%Lp' "${CTX_DATA_ROOT}")"
fi
test "${data_root_mode}" = 700

case " $* " in
  *" --backend semantic "*)
    test "${CTX_ANALYTICS_ENABLED:-}" = false
    test "${CTX_UPGRADE_AUTO:-}" = off
    test "${CTX_SEARCH_SEMANTIC:-}" = 1
    test "${CTX_DAEMON_ENABLED:-}" = 1
    printf '%s\n' 'semantic-only search will not initialize or download intfloat/multilingual-e5-small during search' >&2
    exit 1
    ;;
  *" status --format json "*)
    test -z "${CTX_SEARCH_SEMANTIC:-}"
    if test -n "${CTX_PRO_HELPER:-}"; then
      test "${CTX_ANALYTICS_ENABLED:-}" = false
      test "${CTX_UPGRADE_AUTO:-}" = off
      test "${CTX_DAEMON_ENABLED:-}" = false
    elif test "${CTX_ANALYTICS_ENABLED+x}" != x; then
      test -z "${CTX_UPGRADE_AUTO:-}"
      test -z "${CTX_DAEMON_ENABLED:-}"
    else
      test "${CTX_ANALYTICS_ENABLED:-}" = false
      test "${CTX_UPGRADE_AUTO:-}" = off
      test "${CTX_DAEMON_ENABLED:-}" = false
    fi
    ;;
  *)
    test "${CTX_ANALYTICS_ENABLED:-}" = false
    test "${CTX_UPGRADE_AUTO:-}" = off
    test "${CTX_DAEMON_ENABLED:-}" = false
    test "${CTX_SEARCH_SEMANTIC:-}" = 0
    ;;
esac

case "${1:-}" in
  --version)
    version=0.25.0
    case "$0" in *bad-version*) version=9.9.9 ;; esac
    printf 'ctx %s\n' "${version}"
    ;;
  setup)
    ;;
  import)
    printf '%s\n' '{"totals":{"imported_events":2}}'
    ;;
  search)
    printf '%s\n' '{"retrieval":{"requested_mode":"lexical","effective_mode":"lexical"},"results":[{"text":"Add a parser test."}]}'
    ;;
  status)
    if test -n "${CTX_PRO_HELPER:-}"; then
      printf '%s' '{"read_only":true,"pro":'
      cat "${0}.pro-status.json"
      printf '%s\n' '}'
    elif test "${CTX_ANALYTICS_ENABLED+x}" != x; then
      analytics_path="${CTX_ANALYTICS_ENDPOINT#file://}"
      printf '%s\n' '{"events":[{"event_name":"operation_completed"}]}' > "${analytics_path}"
      cat <<'JSON'
{
  "read_only": true,
  "daemon": {
    "jobs": {
      "history_refresh": {
        "enabled": false
      }
    },
    "enabled": true
  },
  "upgrade": {
    "auto": "apply",
    "auto_enabled": true
  },
  "semantic": {
    "config_source": "default",
    "enabled": false,
    "reason": "semantic_disabled",
    "embed_policy": {
      "source": "dynamic_quiet"
    }
  }
}
JSON
    else
      cat <<'JSON'
{
  "read_only": true,
  "daemon": {
    "jobs": {
      "history_refresh": {
        "enabled": true
      }
    },
    "enabled": false
  },
  "upgrade": {
    "auto": "off",
    "auto_enabled": false
  },
  "semantic": {
    "config_source": "default",
    "enabled": false,
    "reason": "semantic_disabled",
    "embed_policy": {
      "source": "dynamic_quiet"
    }
  }
}
JSON
    fi
    ;;
  *)
    printf 'unexpected fake ctx arguments: %s\n' "$*" >&2
    exit 1
    ;;
esac
EOF
chmod +x "${fake}"
printf '%s\n' '{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}' > "${tmp}/fixture.jsonl"

expected='{"schema_version":1,"kind":"ctx-native-candidate-smoke","status":"passed","steps":{"version":"passed","setup":"passed","import":"passed","search":"passed","read_only":"passed","released_defaults":"passed","explicit_opt_outs":"passed","pro_helper_override_ignored":"passed","semantic_offline_fail_closed":"passed"}}'
for access_case in absent-helper-trial absent-helper-locked absent-helper-unavailable; do
  cp "${pro_status_fixtures}/${access_case}.json" "${fake}.pro-status.json"
  result="${tmp}/result-${access_case}.json"
  "${smoke}" "${fake}" "${tmp}/fixture.jsonl" 0.25.0 "${result}" >/dev/null
  [[ "$(tr -d '\r\n' < "${result}")" == "${expected}" ]] || {
    printf 'candidate smoke result schema changed for %s\n' "${access_case}" >&2
    cat "${result}" >&2
    exit 1
  }
done

failed_result="${tmp}/failed-result.json"
cp "${fake}" "${tmp}/ctx-bad-version"
if "${smoke}" \
  "${tmp}/ctx-bad-version" "${tmp}/fixture.jsonl" 0.25.0 "${failed_result}" \
  >"${tmp}/failure.out" 2>"${tmp}/failure.err"; then
  printf 'candidate smoke accepted a mismatched version\n' >&2
  exit 1
fi
[[ ! -e "${failed_result}" ]] || {
  printf 'candidate smoke wrote passing evidence after failure\n' >&2
  exit 1
}
grep -Fq 'candidate version mismatch' "${tmp}/failure.err"

hung_result="${tmp}/hung-result.json"
cp "${fake}" "${tmp}/ctx-hang"
sed -i '/case "${1:-}" in/i\case "$0" in *ctx-hang) sleep 30 ;; esac' "${tmp}/ctx-hang"
started="$(date +%s)"
if CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS=1 "${smoke}" \
  "${tmp}/ctx-hang" "${tmp}/fixture.jsonl" 0.25.0 "${hung_result}" \
  >"${tmp}/hung.out" 2>"${tmp}/hung.err"; then
  printf 'candidate smoke accepted a hung command\n' >&2
  exit 1
fi
elapsed="$(( $(date +%s) - started ))"
[[ "${elapsed}" -lt 10 ]] || {
  printf 'candidate smoke timeout was not bounded: %ss\n' "${elapsed}" >&2
  exit 1
}
[[ ! -e "${hung_result}" ]]
grep -Fq 'candidate command exceeded 1 seconds' "${tmp}/hung.err"

printf 'native candidate smoke tests passed\n'
