#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/ci-common.sh
source "${script_dir}/ci-common.sh"

usage() {
  cat <<'USAGE'
usage: scripts/release-provider-live-e2e-lanes.sh definitions
       scripts/release-provider-live-e2e-lanes.sh run-selected
       scripts/release-provider-live-e2e-lanes.sh run PROVIDER

Writes non-publishing Buildkite lane definitions for opt-in live provider E2E.
The run modes currently record explicit blockers unless a provider-specific
live runner is implemented by that provider workstream.
USAGE
}

provider_env_name() {
  local provider="$1"

  case "${provider}" in
    codex) printf 'CTX_LIVE_PROVIDER_CODEX' ;;
    claude_code) printf 'CTX_LIVE_PROVIDER_CLAUDE_CODE' ;;
    pi) printf 'CTX_LIVE_PROVIDER_PI' ;;
    open_code) printf 'CTX_LIVE_PROVIDER_OPEN_CODE' ;;
    antigravity_cli) printf 'CTX_LIVE_PROVIDER_ANTIGRAVITY_CLI' ;;
    gemini_cli) printf 'CTX_LIVE_PROVIDER_GEMINI_CLI' ;;
    cursor) printf 'CTX_LIVE_PROVIDER_CURSOR' ;;
    *) return 1 ;;
  esac
}

provider_display_name() {
  local provider="$1"

  case "${provider}" in
    codex) printf 'Codex' ;;
    claude_code) printf 'Claude Code' ;;
    pi) printf 'Pi' ;;
    open_code) printf 'OpenCode' ;;
    antigravity_cli) printf 'Antigravity CLI' ;;
    gemini_cli) printf 'Gemini CLI' ;;
    cursor) printf 'Cursor' ;;
    *) return 1 ;;
  esac
}

provider_secret_scope() {
  local provider="$1"

  printf 'buildkite/provider-live-e2e/%s' "${provider}"
}

provider_ids() {
  printf '%s\n' \
    codex \
    claude_code \
    pi \
    open_code \
    antigravity_cli \
    gemini_cli \
    cursor
}

write_lane_definitions() {
  local out_dir="$1"
  local json markdown generated_at commit branch provider env_name display secret_scope comma

  mkdir -p "${out_dir}"
  json="${out_dir}/provider-live-e2e-lanes.json"
  markdown="${out_dir}/provider-live-e2e-lanes.md"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"

  {
    printf '{\n'
    printf '  "schema_version": 1,\n'
    printf '  "kind": "provider_live_e2e_lane_definitions",\n'
    printf '  "publishing": false,\n'
    printf '  "default_enabled": false,\n'
    printf '  "global_enable_env": "CTX_LIVE_PROVIDER_E2E=1",\n'
    printf '  "blocker_accept_env": "CTX_LIVE_PROVIDER_E2E_ACCEPT_BLOCKERS=1",\n'
    printf '  "git_commit": "%s",\n' "$(ctx_json_escape "${commit}")"
    printf '  "git_branch": "%s",\n' "$(ctx_json_escape "${branch}")"
    printf '  "generated_at_unix_s": %s,\n' "${generated_at}"
    printf '  "lanes": [\n'
    comma=''
    while IFS= read -r provider; do
      env_name="$(provider_env_name "${provider}")"
      display="$(provider_display_name "${provider}")"
      secret_scope="$(provider_secret_scope "${provider}")"
      if [[ -n "${comma}" ]]; then
        printf ',\n'
      fi
      printf '    {\n'
      printf '      "provider": "%s",\n' "$(ctx_json_escape "${provider}")"
      printf '      "display_name": "%s",\n' "$(ctx_json_escape "${display}")"
      printf '      "priority": "p0",\n'
      printf '      "buildkite_step_key": "live-provider-e2e-%s",\n' "$(ctx_json_escape "${provider//_/-}")"
      printf '      "enabled_when": "CTX_LIVE_PROVIDER_E2E=1 and %s=1",\n' "$(ctx_json_escape "${env_name}")"
      printf '      "secret_scope": "%s",\n' "$(ctx_json_escape "${secret_scope}")"
      printf '      "command": "CTX_ARTIFACT_DIR=artifacts/buildkite/provider-live-e2e/%s ./scripts/release-provider-live-e2e-lanes.sh run %s",\n' "$(ctx_json_escape "${provider}")" "$(ctx_json_escape "${provider}")"
      printf '      "expected_artifacts": [\n'
      printf '        "artifacts/buildkite/provider-live-e2e/%s/live-e2e.json",\n' "$(ctx_json_escape "${provider}")"
      printf '        "artifacts/buildkite/provider-live-e2e/%s/live-e2e.md"\n' "$(ctx_json_escape "${provider}")"
      printf '      ],\n'
      printf '      "default_status": "blocked_until_provider_runner_exists",\n'
      printf '      "support_matrix_gate": "docs/provider-support-matrix.json must not mark this provider supported-live without a real live-e2e artifact"\n'
      printf '    }'
      comma=','
    done < <(provider_ids)
    printf '\n'
    printf '  ]\n'
    printf '}\n'
  } > "${json}"

  {
    printf '# Provider Live E2E Lane Definitions\n\n'
    printf '%s\n\n' '- Publishing: false'
    printf '%s `%s`\n' '- Global opt-in:' 'CTX_LIVE_PROVIDER_E2E=1'
    printf '%s `%s`\n\n' '- Blocker acceptance for exploratory runs:' 'CTX_LIVE_PROVIDER_E2E_ACCEPT_BLOCKERS=1'
    printf '| Provider | Enablement | Secret scope | Buildkite key |\n'
    printf '| --- | --- | --- | --- |\n'
    while IFS= read -r provider; do
      env_name="$(provider_env_name "${provider}")"
      display="$(provider_display_name "${provider}")"
      secret_scope="$(provider_secret_scope "${provider}")"
      printf '| %s | `%s=1` | `%s` | `live-provider-e2e-%s` |\n' \
        "${display}" \
        "${env_name}" \
        "${secret_scope}" \
        "${provider//_/-}"
    done < <(provider_ids)
    printf '\n'
    printf 'These definitions are opt-in. Normal CI records the lane contract only;\n'
    printf 'provider workers must replace blocker stubs with real deterministic live E2E commands before a provider is marked `supported-live`.\n'
  } > "${markdown}"

  printf 'provider live E2E lane definitions: %s\n' "${json}"
  printf 'provider live E2E lane notes: %s\n' "${markdown}"
}

write_provider_blocker() {
  local provider="$1"
  local out_dir="$2"
  local json markdown generated_at commit branch env_name display secret_scope

  env_name="$(provider_env_name "${provider}")"
  display="$(provider_display_name "${provider}")"
  secret_scope="$(provider_secret_scope "${provider}")"
  mkdir -p "${out_dir}"
  json="${out_dir}/live-e2e.json"
  markdown="${out_dir}/live-e2e.md"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"

  cat > "${json}" <<EOF
{
  "schema_version": 1,
  "kind": "provider_live_e2e_result",
  "publishing": false,
  "provider": "$(ctx_json_escape "${provider}")",
  "display_name": "$(ctx_json_escape "${display}")",
  "status": "blocked",
  "enabled_by": "CTX_LIVE_PROVIDER_E2E=1 and ${env_name}=1",
  "secret_scope": "$(ctx_json_escape "${secret_scope}")",
  "blocker": "No provider-specific live E2E runner is implemented in this release/CI slice.",
  "next_action": "Provider worker must add a deterministic live run command, evidence assertions, redaction scan, and artifact export before this lane can pass as supported-live proof.",
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at}
}
EOF

  cat > "${markdown}" <<EOF
# ${display} Live E2E Blocker

- Publishing: false
- Provider: \`${provider}\`
- Enabled by: \`CTX_LIVE_PROVIDER_E2E=1 ${env_name}=1\`
- Secret scope: \`${secret_scope}\`
- Status: blocked
- Blocker: no provider-specific live E2E runner is implemented in this release/CI slice.
- Next action: provider worker must add a deterministic live run command, evidence assertions, redaction scan, and artifact export before this lane can pass as supported-live proof.
EOF

  printf 'provider live E2E blocker: %s\n' "${json}"
  if [[ "${CTX_LIVE_PROVIDER_E2E_ACCEPT_BLOCKERS:-0}" == "1" ]]; then
    return 0
  fi
  return 1
}

run_selected() {
  local selected=0 provider env_name provider_dir

  mkdir -p "${CTX_ARTIFACT_DIR}"
  while IFS= read -r provider; do
    env_name="$(provider_env_name "${provider}")"
    if [[ "${!env_name:-0}" == "1" ]]; then
      selected=1
      provider_dir="${CTX_ARTIFACT_DIR}/${provider}"
      write_provider_blocker "${provider}" "${provider_dir}"
    fi
  done < <(provider_ids)

  if (( selected == 0 )); then
    printf '{"schema_version":1,"kind":"provider_live_e2e_result","publishing":false,"status":"skipped","reason":"live provider E2E is opt-in; set CTX_LIVE_PROVIDER_E2E=1 and one CTX_LIVE_PROVIDER_<PROVIDER>=1 variable to run a provider lane"}\n' \
      > "${CTX_ARTIFACT_DIR}/live-e2e-skipped.json"
    printf 'provider live E2E selected no provider lanes\n'
  fi
}

main() {
  local mode="${1:-definitions}"
  local provider="${2:-}"

  cd "${CTX_REPO_ROOT}"
  CTX_ARTIFACT_DIR="${CTX_ARTIFACT_DIR:-target/ctx-artifacts/provider-live-e2e-lanes}"
  ctx_timing_init
  trap ctx_timing_finish EXIT

  case "${mode}" in
    definitions)
      ctx_run_timed "provider-live-e2e-lane-definitions" write_lane_definitions "${CTX_ARTIFACT_DIR}"
      ;;
    run-selected)
      ctx_run_timed "provider-live-e2e-run-selected" run_selected
      ;;
    run)
      if [[ -z "${provider}" ]]; then
        usage >&2
        return 2
      fi
      provider_env_name "${provider}" >/dev/null
      ctx_run_timed "provider-live-e2e-${provider}" write_provider_blocker "${provider}" "${CTX_ARTIFACT_DIR}"
      ;;
    -h|--help|help)
      usage
      ;;
    *)
      printf 'unknown provider live E2E mode: %s\n' "${mode}" >&2
      usage >&2
      return 2
      ;;
  esac
}

main "$@"
