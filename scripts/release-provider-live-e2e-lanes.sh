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
Codex and Pi can run an explicit local-history import/search smoke.
Providers without native local importers record blockers. Missing opt-in records
skipped artifacts.

Required live env for Codex:
  CTX_LIVE_PROVIDER_E2E=1
  CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY=1
  CTX_LIVE_PROVIDER_CODEX=1
  CTX_LIVE_PROVIDER_CODEX_SESSIONS_PATH=/path/to/.codex/sessions
  CTX_LIVE_PROVIDER_CODEX_QUERY='private local query'
  optional CTX_LIVE_PROVIDER_CODEX_HISTORY_PATH=/path/to/.codex/history.jsonl

Required live env for Pi:
  CTX_LIVE_PROVIDER_E2E=1
  CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY=1
  CTX_LIVE_PROVIDER_PI=1
  CTX_LIVE_PROVIDER_PI_SESSIONS_PATH=/path/to/.pi/sessions.jsonl
  CTX_LIVE_PROVIDER_PI_QUERY='private local query'

Optional runner env:
  CTX_LIVE_PROVIDER_CTX_BIN=/path/to/ctx

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
    copilot_cli) printf 'CTX_LIVE_PROVIDER_COPILOT_CLI' ;;
    factory_ai_droid) printf 'CTX_LIVE_PROVIDER_FACTORY_AI_DROID' ;;
    amp) printf 'CTX_LIVE_PROVIDER_AMP' ;;
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
    copilot_cli) printf 'Copilot CLI' ;;
    factory_ai_droid) printf 'Factory AI Droid' ;;
    amp) printf 'Amp' ;;
    *) return 1 ;;
  esac
}

provider_secret_scope() {
  printf 'none'
}

provider_ids() {
  printf '%s\n' \
    codex \
    claude_code \
    pi \
    open_code \
    antigravity_cli \
    gemini_cli \
    cursor \
    copilot_cli \
    factory_ai_droid \
    amp
}

provider_live_capability() {
  local provider="$1"

  case "${provider}" in
    codex|pi) printf 'local_history_smoke' ;;
    *) printf 'fixture_only_blocker' ;;
  esac
}

provider_required_path_env() {
  local provider="$1"

  case "${provider}" in
    codex) printf 'CTX_LIVE_PROVIDER_CODEX_SESSIONS_PATH' ;;
    pi) printf 'CTX_LIVE_PROVIDER_PI_SESSIONS_PATH' ;;
    *) return 1 ;;
  esac
}

provider_optional_path_env() {
  local provider="$1"

  case "${provider}" in
    codex) printf 'CTX_LIVE_PROVIDER_CODEX_HISTORY_PATH' ;;
    pi) printf '' ;;
    *) printf '' ;;
  esac
}

provider_query_env() {
  local provider="$1"

  case "${provider}" in
    codex) printf 'CTX_LIVE_PROVIDER_CODEX_QUERY' ;;
    pi) printf 'CTX_LIVE_PROVIDER_PI_QUERY' ;;
    *) return 1 ;;
  esac
}

ctx_path_kind() {
  local path="$1"

  if [[ -d "${path}" ]]; then
    printf 'directory'
  elif [[ -f "${path}" ]]; then
    printf 'file'
  else
    printf 'missing'
  fi
}

ctx_bool() {
  if [[ "${1:-0}" == "1" || "${1:-}" == "true" ]]; then
    printf 'true'
  else
    printf 'false'
  fi
}

require_python3() {
  if command -v python3 >/dev/null 2>&1; then
    return 0
  fi
  printf 'python3 is required for redacted live E2E JSON aggregation\n' >&2
  return 127
}

json_int() {
  local file="$1"
  local path="$2"

  python3 - "${file}" "${path}" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    value = json.load(handle)

for part in sys.argv[2].split("."):
    if not part:
        continue
    if part.endswith("[]"):
        value = len(value.get(part[:-2], []))
    else:
        value = value.get(part, 0)

if isinstance(value, bool):
    print(1 if value else 0)
elif isinstance(value, int):
    print(value)
elif value is None:
    print(0)
else:
    print(int(value))
PY
}

json_bool() {
  local file="$1"
  local path="$2"

  python3 - "${file}" "${path}" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    value = json.load(handle)

for part in sys.argv[2].split("."):
    if not part:
        continue
    value = value.get(part)

print("true" if value is True else "false")
PY
}

json_string() {
  local file="$1"
  local path="$2"

  python3 - "${file}" "${path}" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    value = json.load(handle)

for part in sys.argv[2].split("."):
    if not part:
        continue
    value = value.get(part)

if value is None:
    print("")
else:
    print(str(value))
PY
}

artifact_status() {
  local json="$1"

  sed -n 's/^[[:space:]]*"status": "\([^"]*\)".*/\1/p' "${json}" | head -n1
}

find_ctx_bin() {
  local candidate suffix

  suffix="$(ctx_host_exe_suffix)"
  for candidate in \
    "${CTX_LIVE_PROVIDER_CTX_BIN:-}" \
    "${CTX_BIN:-}" \
    "${CTX_REPO_ROOT}/target/debug/ctx${suffix}" \
    "${CTX_REPO_ROOT}/target/release/ctx${suffix}"
  do
    if [[ -n "${candidate}" && ( -x "${candidate}" || -f "${candidate}" ) ]]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done

  if command -v ctx >/dev/null 2>&1; then
    command -v ctx
    return 0
  fi

  return 1
}

write_redacted_ctx_stderr_summary() {
  local raw_stderr="$1"
  local summary="$2"
  local command_name="$3"
  local exit_status="$4"
  local stderr_lines stderr_bytes

  if [[ ! "${command_name}" =~ ^[A-Za-z0-9_-]+$ ]]; then
    command_name="ctx"
  fi
  stderr_lines="$(wc -l < "${raw_stderr}" | tr -d '[:space:]')"
  stderr_bytes="$(wc -c < "${raw_stderr}" | tr -d '[:space:]')"

  cat > "${summary}" <<EOF
ctx_command: ${command_name}
exit_status: ${exit_status}
stderr_lines: ${stderr_lines}
stderr_bytes: ${stderr_bytes}
stderr_content: suppressed; raw stderr may contain local paths, queries, snippets, or environment values
EOF
}

run_ctx_json() {
  local ctx_bin="$1"
  local data_root="$2"
  local safe_home="$3"
  local output="$4"
  local stderr_raw stderr_summary status command_name had_errexit=0
  shift 4

  stderr_raw="${output}.stderr.raw"
  stderr_summary="${output}.stderr.redacted"
  command_name="${1:-ctx}"
  CTX_LIVE_LAST_STDERR_SUMMARY="${stderr_summary}"

  [[ $- == *e* ]] && had_errexit=1
  set +e
  env -i \
    PATH="${PATH}" \
    HOME="${safe_home}" \
    CTX_DATA_ROOT="${data_root}" \
    LANG="${LANG:-C}" \
    LC_ALL="${LC_ALL:-C}" \
    "${ctx_bin}" "$@" > "${output}" 2> "${stderr_raw}"
  status=$?

  if (( status != 0 )); then
    write_redacted_ctx_stderr_summary "${stderr_raw}" "${stderr_summary}" "${command_name}" "${status}"
    printf 'ctx %s failed; stderr captured and redacted before logging or artifact export\n' "${command_name}" >&2
    cat "${stderr_summary}" >&2
  else
    : > "${stderr_summary}"
  fi
  rm -f "${stderr_raw}"
  if (( had_errexit == 1 )); then
    set -e
  else
    set +e
  fi
  return "${status}"
}

artifact_guard_no_raw_values() {
  local json="$1"
  local markdown="$2"
  shift 2
  local value

  for value in "$@"; do
    if [[ -z "${value}" ]]; then
      continue
    fi
    if grep -F -- "${value}" "${json}" "${markdown}" >/dev/null 2>&1; then
      printf 'live E2E artifact redaction failed: raw opt-in value appeared in artifact\n' >&2
      return 1
    fi
  done
}

json_provider_retrieval_oracle() {
  local provider="$1"
  local search_json="$2"
  local require_source_exists="${3:-1}"

  python3 - "${provider}" "${search_json}" "${require_source_exists}" <<'PY'
import json
import sys

provider = sys.argv[1]
require_source_exists = sys.argv[3] == "1"
with open(sys.argv[2], encoding="utf-8") as handle:
    search = json.load(handle)


def results(packet):
    values = packet.get("results", [])
    return values if isinstance(values, list) else []


def result_counts(values, *, require_result_provider):
    out = {
        "results": len(values),
        "provider_matches": 0,
        "provider_mismatches": 0,
        "provider_missing": 0,
        "source_exists_true": 0,
        "source_exists_false": 0,
        "source_exists_missing": 0,
        "citation_count": 0,
        "citation_provider_matches": 0,
        "citation_provider_mismatches": 0,
        "citation_provider_missing": 0,
        "citation_source_exists_true": 0,
        "citation_source_exists_false": 0,
        "citation_source_exists_missing": 0,
        "results_with_provider_citation": 0,
        "results_with_source_exists_citation": 0,
    }
    for result in values:
        if require_result_provider:
            if result.get("provider") == provider:
                out["provider_matches"] += 1
            elif "provider" in result:
                out["provider_mismatches"] += 1
            else:
                out["provider_missing"] += 1

            if result.get("source_exists") is True:
                out["source_exists_true"] += 1
            elif result.get("source_exists") is False:
                out["source_exists_false"] += 1
            else:
                out["source_exists_missing"] += 1

        citations = result.get("citations", [])
        if not isinstance(citations, list):
            citations = []
        has_provider_citation = False
        has_source_exists_citation = False
        for citation in citations:
            out["citation_count"] += 1
            if citation.get("provider") == provider:
                out["citation_provider_matches"] += 1
                has_provider_citation = True
            elif "provider" in citation:
                out["citation_provider_mismatches"] += 1
            else:
                out["citation_provider_missing"] += 1

            if citation.get("source_exists") is True:
                out["citation_source_exists_true"] += 1
                has_source_exists_citation = True
            elif citation.get("source_exists") is False:
                out["citation_source_exists_false"] += 1
            else:
                out["citation_source_exists_missing"] += 1
        if has_provider_citation:
            out["results_with_provider_citation"] += 1
        if has_source_exists_citation:
            out["results_with_source_exists_citation"] += 1
    return out


search_counts = result_counts(results(search), require_result_provider=True)
passed = (
    search_counts["results"] > 0
    and search_counts["provider_matches"] == search_counts["results"]
    and search_counts["provider_mismatches"] == 0
    and search_counts["provider_missing"] == 0
    and search_counts["citation_count"] > 0
    and search_counts["citation_provider_matches"] == search_counts["citation_count"]
    and (
        not require_source_exists
        or (
            search_counts["source_exists_true"] == search_counts["results"]
            and search_counts["citation_source_exists_true"] == search_counts["citation_count"]
        )
    )
)
print(json.dumps({
    "passed": passed,
    "expected_search_results_min": 1,
    "search": search_counts,
}, sort_keys=True))
PY
}

write_lane_definitions() {
  local out_dir="$1"
  local json markdown generated_at commit branch provider env_name display secret_scope comma
  local capability required_path_env optional_path_env query_env

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
    printf '  "local_history_accept_env": "CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY=1",\n'
    printf '  "blocker_accept_env": "CTX_LIVE_PROVIDER_E2E_ACCEPT_BLOCKERS=1",\n'
    printf '  "provider_command_execution": false,\n'
    printf '  "api_key_env_passed_to_ctx": false,\n'
    printf '  "artifact_redaction": "aggregate_and_oracle_counts_only_no_raw_transcripts_snippets_queries_or_source_paths",\n'
    printf '  "git_commit": "%s",\n' "$(ctx_json_escape "${commit}")"
    printf '  "git_branch": "%s",\n' "$(ctx_json_escape "${branch}")"
    printf '  "generated_at_unix_s": %s,\n' "${generated_at}"
    printf '  "lanes": [\n'
    comma=''
    while IFS= read -r provider; do
      env_name="$(provider_env_name "${provider}")"
      display="$(provider_display_name "${provider}")"
      secret_scope="$(provider_secret_scope "${provider}")"
      capability="$(provider_live_capability "${provider}")"
      required_path_env="$(provider_required_path_env "${provider}" 2>/dev/null || true)"
      optional_path_env="$(provider_optional_path_env "${provider}")"
      query_env="$(provider_query_env "${provider}" 2>/dev/null || true)"
      if [[ -n "${comma}" ]]; then
        printf ',\n'
      fi
      printf '    {\n'
      printf '      "provider": "%s",\n' "$(ctx_json_escape "${provider}")"
      printf '      "display_name": "%s",\n' "$(ctx_json_escape "${display}")"
      printf '      "priority": "p0",\n'
      printf '      "buildkite_step_key": "live-provider-e2e-%s",\n' "$(ctx_json_escape "${provider//_/-}")"
      if [[ "${capability}" == "local_history_smoke" ]]; then
        printf '      "capability": "local_history_import_search_smoke",\n'
        printf '      "enabled_when": "CTX_LIVE_PROVIDER_E2E=1 and CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY=1 and %s=1 and %s is set and %s or CTX_LIVE_PROVIDER_QUERY is set",\n' "$(ctx_json_escape "${env_name}")" "$(ctx_json_escape "${required_path_env}")" "$(ctx_json_escape "${query_env}")"
      else
        printf '      "capability": "fixture_only_blocker",\n'
        printf '      "enabled_when": "CTX_LIVE_PROVIDER_E2E=1 and %s=1",\n' "$(ctx_json_escape "${env_name}")"
      fi
      printf '      "secret_scope": "%s",\n' "$(ctx_json_escape "${secret_scope}")"
      printf '      "requires_provider_command_execution": false,\n'
      printf '      "passes_api_key_env_to_ctx": false,\n'
      if [[ "${capability}" == "local_history_smoke" ]]; then
        printf '      "artifact_redaction": "aggregate_and_oracle_counts_only",\n'
      else
        printf '      "artifact_redaction": "aggregate_counts_only",\n'
      fi
      printf '      "command": "CTX_ARTIFACT_DIR=artifacts/buildkite/provider-live-e2e/%s ./scripts/release-provider-live-e2e-lanes.sh run %s",\n' "$(ctx_json_escape "${provider}")" "$(ctx_json_escape "${provider}")"
      printf '      "expected_artifacts": [\n'
      printf '        "artifacts/buildkite/provider-live-e2e/%s/live-e2e.json",\n' "$(ctx_json_escape "${provider}")"
      printf '        "artifacts/buildkite/provider-live-e2e/%s/live-e2e.md"' "$(ctx_json_escape "${provider}")"
      printf '\n'
      printf '      ],\n'
      if [[ "${capability}" == "local_history_smoke" ]]; then
        printf '      "required_path_env": "%s",\n' "$(ctx_json_escape "${required_path_env}")"
        printf '      "required_query_env": "%s or CTX_LIVE_PROVIDER_QUERY",\n' "$(ctx_json_escape "${query_env}")"
        if [[ -n "${optional_path_env}" ]]; then
          printf '      "optional_path_env": "%s",\n' "$(ctx_json_escape "${optional_path_env}")"
        fi
        printf '      "default_status": "skipped_until_explicit_local_history_opt_in",\n'
      else
        printf '      "default_status": "skipped_until_explicit_provider_opt_in",\n'
        printf '      "selected_status": "blocked_fixture_only_provider",\n'
      fi
      printf '      "support_matrix_gate": "docs/provider-support-matrix.json must not mark this provider supported-live without a redacted live-e2e artifact from this lane"\n'
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
    printf '%s `%s`\n' '- Local-history acceptance:' 'CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY=1'
    printf '%s `%s`\n\n' '- Blocker acceptance for exploratory runs:' 'CTX_LIVE_PROVIDER_E2E_ACCEPT_BLOCKERS=1'
    printf '%s\n\n' '- Provider command execution: false'
    printf '%s\n\n' '- API key environment passed to ctx: false'
    printf '| Provider | Enablement | Capability | Secret scope | Buildkite key |\n'
    printf '| --- | --- | --- | --- | --- |\n'
    while IFS= read -r provider; do
      env_name="$(provider_env_name "${provider}")"
      display="$(provider_display_name "${provider}")"
      secret_scope="$(provider_secret_scope "${provider}")"
      capability="$(provider_live_capability "${provider}")"
      if [[ "${capability}" == "local_history_smoke" ]]; then
        required_path_env="$(provider_required_path_env "${provider}")"
        query_env="$(provider_query_env "${provider}")"
        printf '| %s | `%s=1`, `%s=1`, `%s=1`, `%s` set, `%s` or `%s` set | local-history import/search | `%s` | `live-provider-e2e-%s` |\n' \
          "${display}" \
          "CTX_LIVE_PROVIDER_E2E" \
          "CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY" \
          "${env_name}" \
          "${required_path_env}" \
          "${query_env}" \
          "CTX_LIVE_PROVIDER_QUERY" \
          "${secret_scope}" \
          "${provider//_/-}"
      else
        printf '| %s | `%s=1`, `%s=1` | native-import blocker | `%s` | `live-provider-e2e-%s` |\n' \
          "${display}" \
          "CTX_LIVE_PROVIDER_E2E" \
          "${env_name}" \
          "${secret_scope}" \
          "${provider//_/-}"
      fi
    done < <(provider_ids)
    printf '\n'
    printf 'Codex and Pi lanes use only explicit local-history paths, a temporary `CTX_DATA_ROOT`, and redacted aggregate/oracle-count artifacts.\n'
    printf 'OpenRouter-generated histories are a developer drafting/static-fixture aid only; they are not provider-live lanes, release evidence, or native local-history proof.\n'
    printf 'Providers without native local importers remain blockers for native local-history proof until the public CLI ships a native importer.\n'
  } > "${markdown}"

  printf 'provider live E2E lane definitions: %s\n' "${json}"
  printf 'provider live E2E lane notes: %s\n' "${markdown}"
}

write_skipped_result() {
  local provider="$1"
  local out_dir="$2"
  local reason_code="$3"
  local reason_text="$4"
  local json markdown generated_at commit branch env_name display enabled_by

  env_name="$(provider_env_name "${provider}")"
  display="$(provider_display_name "${provider}")"
  enabled_by="CTX_LIVE_PROVIDER_E2E=1, CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY=1, and ${env_name}=1"
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
  "status": "skipped",
  "reason_code": "$(ctx_json_escape "${reason_code}")",
  "reason": "$(ctx_json_escape "${reason_text}")",
  "enabled_by": "$(ctx_json_escape "${enabled_by}")",
  "provider_command_execution": false,
  "api_key_env_passed_to_ctx": false,
  "artifact_redaction": "aggregate_counts_only_no_raw_transcripts_snippets_queries_or_source_paths",
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at}
}
EOF

  cat > "${markdown}" <<EOF
# ${display} Live E2E Skipped

- Publishing: false
- Provider: \`${provider}\`
- Status: skipped
- Reason: ${reason_text}
- Provider command execution: false
- API key environment passed to ctx: false
- Artifact redaction: aggregate counts only; no raw transcripts, snippets, queries, or source paths.
EOF

  printf 'provider live E2E skipped: %s\n' "${json}"
}

write_selected_skip() {
  local out_dir="$1"
  local reason_code="$2"
  local reason_text="$3"
  local json markdown generated_at commit branch

  mkdir -p "${out_dir}"
  json="${out_dir}/live-e2e.json"
  markdown="${out_dir}/live-e2e.md"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"

  cat > "${json}" <<EOF
{
  "schema_version": 1,
  "kind": "provider_live_e2e_selected_result",
  "publishing": false,
  "status": "skipped",
  "reason_code": "$(ctx_json_escape "${reason_code}")",
  "reason": "$(ctx_json_escape "${reason_text}")",
  "selected_providers": 0,
  "provider_command_execution": false,
  "api_key_env_passed_to_ctx": false,
  "artifact_redaction": "aggregate_counts_only_no_raw_transcripts_snippets_queries_or_source_paths",
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at}
}
EOF

  cat > "${markdown}" <<EOF
# Provider Live E2E Selected Run Skipped

- Publishing: false
- Status: skipped
- Reason: ${reason_text}
- Selected providers: 0
- Provider command execution: false
- API key environment passed to ctx: false
- Artifact redaction: aggregate counts only; no raw transcripts, snippets, queries, or source paths.
EOF

  printf 'provider live E2E selected skipped: %s\n' "${json}"
}

write_selected_summary() {
  local out_dir="$1"
  local selected="$2"
  local passed="$3"
  local skipped="$4"
  local blocked="$5"
  local failed="$6"
  local json markdown generated_at commit branch status

  mkdir -p "${out_dir}"
  json="${out_dir}/live-e2e.json"
  markdown="${out_dir}/live-e2e.md"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"
  status="passed"
  if (( failed > 0 )); then
    status="failed"
  elif (( blocked > 0 )); then
    status="blocked"
  elif (( passed == 0 )); then
    status="skipped"
  fi

  cat > "${json}" <<EOF
{
  "schema_version": 1,
  "kind": "provider_live_e2e_selected_result",
  "publishing": false,
  "status": "${status}",
  "selected_providers": ${selected},
  "providers_passed": ${passed},
  "providers_skipped": ${skipped},
  "providers_blocked": ${blocked},
  "providers_failed": ${failed},
  "provider_command_execution": false,
  "api_key_env_passed_to_ctx": false,
  "artifact_redaction": "aggregate_counts_only_no_raw_transcripts_snippets_queries_or_source_paths",
  "provider_artifacts": "per-provider subdirectories under this artifact directory",
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at}
}
EOF

  cat > "${markdown}" <<EOF
# Provider Live E2E Selected Run

- Publishing: false
- Status: ${status}
- Selected providers: ${selected}
- Passed: ${passed}
- Skipped: ${skipped}
- Blocked: ${blocked}
- Failed: ${failed}
- Provider command execution: false
- API key environment passed to ctx: false
- Artifact redaction: aggregate counts only; no raw transcripts, snippets, queries, or source paths.
- Provider artifacts: per-provider subdirectories under this artifact directory.
EOF

  printf 'provider live E2E selected summary: %s\n' "${json}"
}

write_provider_blocker() {
  local provider="$1"
  local out_dir="$2"
  local json markdown generated_at commit branch env_name display

  env_name="$(provider_env_name "${provider}")"
  display="$(provider_display_name "${provider}")"
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
  "secret_scope": "none",
  "provider_command_execution": false,
  "api_key_env_passed_to_ctx": false,
  "artifact_redaction": "aggregate_counts_only_no_raw_transcripts_snippets_queries_or_source_paths",
  "blocker": "Provider has no native local-history importer in the public CLI; normalized provider JSONL import is not native local-history proof.",
  "next_action": "Add a public read-only native local-history importer, update docs/provider-support-matrix.json, and produce a redacted local-history live E2E artifact before this provider can be treated as live-supported.",
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
- Secret scope: \`none\`
- Status: blocked
- Provider command execution: false
- API key environment passed to ctx: false
- Artifact redaction: aggregate counts only; no raw transcripts, snippets, queries, or source paths.
- Blocker: provider has no native local-history importer in the public CLI; normalized provider JSONL import is not native local-history proof.
- Next action: add a public read-only native local-history importer, update \`docs/provider-support-matrix.json\`, and produce a redacted local-history live E2E artifact before this provider can be treated as live-supported.
EOF

  printf 'provider live E2E blocker: %s\n' "${json}"
  if [[ "${CTX_LIVE_PROVIDER_E2E_ACCEPT_BLOCKERS:-0}" == "1" ]]; then
    return 0
  fi
  return 1
}

run_fixture_only_provider() {
  local provider="$1"
  local out_dir="$2"
  local env_name

  env_name="$(provider_env_name "${provider}")"
  if [[ "${CTX_LIVE_PROVIDER_E2E:-0}" != "1" ]]; then
    write_skipped_result "${provider}" "${out_dir}" "global_opt_in_missing" \
      "live provider E2E is opt-in; set CTX_LIVE_PROVIDER_E2E=1 to run this lane"
    return 0
  fi
  if [[ "${!env_name:-0}" != "1" ]]; then
    write_skipped_result "${provider}" "${out_dir}" "provider_opt_in_missing" \
      "provider lane is opt-in; set ${env_name}=1 to run this provider"
    return 0
  fi

  write_provider_blocker "${provider}" "${out_dir}"
}

write_live_failure_result() {
  local provider="$1"
  local out_dir="$2"
  local reason_code="$3"
  local reason_text="$4"
  local stderr_summary="${5:-}"
  local json markdown generated_at commit branch display error_artifact_json error_artifact_markdown

  display="$(provider_display_name "${provider}")"
  mkdir -p "${out_dir}"
  json="${out_dir}/live-e2e.json"
  markdown="${out_dir}/live-e2e.md"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"
  error_artifact_json=""
  error_artifact_markdown=""
  if [[ -n "${stderr_summary}" && -s "${stderr_summary}" ]]; then
    cp "${stderr_summary}" "${out_dir}/live-e2e-error.txt"
    error_artifact_json='  "redacted_error_artifact": "live-e2e-error.txt",'
    error_artifact_markdown='- Redacted error artifact: `live-e2e-error.txt`'
  fi

  cat > "${json}" <<EOF
{
  "schema_version": 1,
  "kind": "provider_live_e2e_result",
  "publishing": false,
  "provider": "$(ctx_json_escape "${provider}")",
  "display_name": "$(ctx_json_escape "${display}")",
  "status": "failed",
  "reason_code": "$(ctx_json_escape "${reason_code}")",
  "reason": "$(ctx_json_escape "${reason_text}")",
  "provider_command_execution": false,
  "api_key_env_passed_to_ctx": false,
  "artifact_redaction": "aggregate_counts_only_no_raw_transcripts_snippets_queries_or_source_paths",
${error_artifact_json}
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at}
}
EOF

  cat > "${markdown}" <<EOF
# ${display} Live E2E Failed

- Publishing: false
- Provider: \`${provider}\`
- Status: failed
- Reason: ${reason_text}
- Provider command execution: false
- API key environment passed to ctx: false
- Artifact redaction: aggregate counts only; no raw transcripts, snippets, queries, or source paths.
${error_artifact_markdown}
EOF

  printf 'provider live E2E failed: %s\n' "${json}"
}

run_local_history_provider() {
  local provider="$1"
  local out_dir="$2"
  local env_name display required_path_env optional_path_env query_env
  local required_path optional_path query configured_query raw_query_guard ctx_bin tmp_root data_root safe_home
  local setup_json import_json search_json status_json doctor_json validate_json
  local imported_source_files=0 imported_source_bytes=0 imported_sessions=0 imported_events=0 imported_edges=0 import_skipped=0 import_failed=0
  local extra_files extra_bytes extra_sessions extra_events extra_edges extra_skipped extra_failed optional_import_json
  local search_results indexed_items indexed_sources doctor_ok validate_valid
  local oracle_json oracle_pass
  local oracle_search_provider_matches oracle_search_provider_mismatches oracle_search_provider_missing
  local oracle_search_source_exists_true oracle_search_source_exists_false oracle_search_source_exists_missing
  local oracle_search_citation_count oracle_search_citation_provider_matches oracle_search_citation_provider_mismatches oracle_search_citation_provider_missing
  local oracle_search_citation_source_exists_true oracle_search_citation_source_exists_false oracle_search_citation_source_exists_missing
  local source_inputs=1 source_paths_configured=1 optional_source_configured=0 json markdown generated_at commit branch
  local required_kind optional_kind setup_status import_status optional_import_status search_status status_status doctor_status validate_status
  local failed_stderr_summary

  env_name="$(provider_env_name "${provider}")"
  display="$(provider_display_name "${provider}")"
  required_path_env="$(provider_required_path_env "${provider}")"
  optional_path_env="$(provider_optional_path_env "${provider}")"
  query_env="$(provider_query_env "${provider}")"
  required_path="${!required_path_env:-}"
  optional_path=""
  if [[ -n "${optional_path_env}" ]]; then
    optional_path="${!optional_path_env:-}"
  fi
  query="${!query_env:-${CTX_LIVE_PROVIDER_QUERY:-}}"
  configured_query="$(ctx_bool "$([[ -n "${query}" ]] && printf 1 || printf 0)")"
  raw_query_guard="${query}"

  if [[ "${CTX_LIVE_PROVIDER_E2E:-0}" != "1" ]]; then
    write_skipped_result "${provider}" "${out_dir}" "global_opt_in_missing" \
      "live provider E2E is opt-in; set CTX_LIVE_PROVIDER_E2E=1 to run this lane"
    return 0
  fi
  if [[ "${!env_name:-0}" != "1" ]]; then
    write_skipped_result "${provider}" "${out_dir}" "provider_opt_in_missing" \
      "provider lane is opt-in; set ${env_name}=1 to run this provider"
    return 0
  fi
  if [[ "${CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY:-0}" != "1" ]]; then
    write_skipped_result "${provider}" "${out_dir}" "local_history_acceptance_missing" \
      "real local-history access requires CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY=1"
    return 0
  fi
  if [[ -z "${required_path}" ]]; then
    write_skipped_result "${provider}" "${out_dir}" "required_path_env_missing" \
      "required local-history path environment variable ${required_path_env} is not set"
    return 0
  fi
  if [[ -z "${query}" ]]; then
    write_skipped_result "${provider}" "${out_dir}" "query_env_missing" \
      "provider-specific query environment variable ${query_env} or CTX_LIVE_PROVIDER_QUERY is required for deterministic retrieval oracles"
    return 0
  fi
  if [[ ! -e "${required_path}" ]]; then
    write_live_failure_result "${provider}" "${out_dir}" "required_path_missing" \
      "configured required local-history path does not exist"
    return 1
  fi
  if [[ -n "${optional_path}" && ! -e "${optional_path}" ]]; then
    write_live_failure_result "${provider}" "${out_dir}" "optional_path_missing" \
      "configured optional local-history path does not exist"
    return 1
  fi
  if ! require_python3; then
    write_live_failure_result "${provider}" "${out_dir}" "python3_missing" \
      "python3 is required to parse private ctx command JSON into redacted aggregate artifacts"
    return 1
  fi
  if ! ctx_bin="$(find_ctx_bin)"; then
    write_live_failure_result "${provider}" "${out_dir}" "ctx_binary_missing" \
      "ctx binary was not found; set CTX_LIVE_PROVIDER_CTX_BIN to an existing ctx binary"
    return 1
  fi

  mkdir -p "${out_dir}" "${TMPDIR:-${CTX_REPO_ROOT}/target/tmp}"
  tmp_root="$(mktemp -d "${TMPDIR:-${CTX_REPO_ROOT}/target/tmp}/ctx-live-e2e-${provider}.XXXXXX")"
  data_root="${tmp_root}/ctx-data"
  safe_home="${tmp_root}/home"
  mkdir -p "${safe_home}"

  setup_json="${tmp_root}/setup.json"
  import_json="${tmp_root}/import.json"
  search_json="${tmp_root}/search.json"
  status_json="${tmp_root}/status.json"
  doctor_json="${tmp_root}/doctor.json"
  validate_json="${tmp_root}/validate.json"

  set +e
  run_ctx_json "${ctx_bin}" "${data_root}" "${safe_home}" "${setup_json}" setup --json
  setup_status=$?
  set -e
  if (( setup_status != 0 )); then
    write_live_failure_result "${provider}" "${out_dir}" "ctx_setup_failed" \
      "ctx setup failed while using a temporary CTX_DATA_ROOT" \
      "${CTX_LIVE_LAST_STDERR_SUMMARY}"
    rm -rf "${tmp_root}"
    return 1
  fi

  set +e
  run_ctx_json "${ctx_bin}" "${data_root}" "${safe_home}" "${import_json}" import --provider "${provider}" --path "${required_path}" --json
  import_status=$?
  set -e
  if (( import_status != 0 )); then
    write_live_failure_result "${provider}" "${out_dir}" "ctx_import_failed" \
      "ctx import failed for the configured local-history path" \
      "${CTX_LIVE_LAST_STDERR_SUMMARY}"
    rm -rf "${tmp_root}"
    return 1
  fi

  imported_source_files="$(json_int "${import_json}" "totals.source_files")"
  imported_source_bytes="$(json_int "${import_json}" "totals.source_bytes")"
  imported_sessions="$(json_int "${import_json}" "totals.imported_sessions")"
  imported_events="$(json_int "${import_json}" "totals.imported_events")"
  imported_edges="$(json_int "${import_json}" "totals.imported_edges")"
  import_skipped="$(json_int "${import_json}" "totals.skipped")"
  import_failed="$(json_int "${import_json}" "totals.failed")"

  if [[ -n "${optional_path}" ]]; then
    source_inputs=2
    source_paths_configured=2
    optional_source_configured=1
    optional_import_json="${tmp_root}/import-optional.json"
    set +e
    run_ctx_json "${ctx_bin}" "${data_root}" "${safe_home}" "${optional_import_json}" import --provider "${provider}" --path "${optional_path}" --json
    optional_import_status=$?
    set -e
    if (( optional_import_status != 0 )); then
      write_live_failure_result "${provider}" "${out_dir}" "ctx_optional_import_failed" \
        "ctx import failed for the configured optional local-history path" \
        "${CTX_LIVE_LAST_STDERR_SUMMARY}"
      rm -rf "${tmp_root}"
      return 1
    fi
    extra_files="$(json_int "${optional_import_json}" "totals.source_files")"
    extra_bytes="$(json_int "${optional_import_json}" "totals.source_bytes")"
    extra_sessions="$(json_int "${optional_import_json}" "totals.imported_sessions")"
    extra_events="$(json_int "${optional_import_json}" "totals.imported_events")"
    extra_edges="$(json_int "${optional_import_json}" "totals.imported_edges")"
    extra_skipped="$(json_int "${optional_import_json}" "totals.skipped")"
    extra_failed="$(json_int "${optional_import_json}" "totals.failed")"
    imported_source_files=$(( imported_source_files + extra_files ))
    imported_source_bytes=$(( imported_source_bytes + extra_bytes ))
    imported_sessions=$(( imported_sessions + extra_sessions ))
    imported_events=$(( imported_events + extra_events ))
    imported_edges=$(( imported_edges + extra_edges ))
    import_skipped=$(( import_skipped + extra_skipped ))
    import_failed=$(( import_failed + extra_failed ))
  fi

  if (( imported_sessions == 0 || imported_events == 0 )); then
    rm -rf "${tmp_root}"
    write_live_failure_result "${provider}" "${out_dir}" "no_imported_history" \
      "ctx import completed but did not import both sessions and events"
    return 1
  fi

  set +e
  failed_stderr_summary=""
  run_ctx_json "${ctx_bin}" "${data_root}" "${safe_home}" "${search_json}" search "${query}" --provider "${provider}" --limit 5 --json
  search_status=$?
  if (( search_status != 0 )); then
    failed_stderr_summary="${CTX_LIVE_LAST_STDERR_SUMMARY}"
  fi
  run_ctx_json "${ctx_bin}" "${data_root}" "${safe_home}" "${status_json}" status --json
  status_status=$?
  if (( status_status != 0 && -z "${failed_stderr_summary}" )); then
    failed_stderr_summary="${CTX_LIVE_LAST_STDERR_SUMMARY}"
  fi
  run_ctx_json "${ctx_bin}" "${data_root}" "${safe_home}" "${doctor_json}" doctor --json
  doctor_status=$?
  if (( doctor_status != 0 && -z "${failed_stderr_summary}" )); then
    failed_stderr_summary="${CTX_LIVE_LAST_STDERR_SUMMARY}"
  fi
  run_ctx_json "${ctx_bin}" "${data_root}" "${safe_home}" "${validate_json}" validate --json
  validate_status=$?
  if (( validate_status != 0 && -z "${failed_stderr_summary}" )); then
    failed_stderr_summary="${CTX_LIVE_LAST_STDERR_SUMMARY}"
  fi
  set -e
  if (( search_status != 0 || status_status != 0 || doctor_status != 0 || validate_status != 0 )); then
    write_live_failure_result "${provider}" "${out_dir}" "ctx_retrieval_or_health_failed" \
      "ctx search, status, doctor, or validate failed after import" \
      "${failed_stderr_summary}"
    rm -rf "${tmp_root}"
    return 1
  fi

  search_results="$(json_int "${search_json}" "results[]")"
  indexed_items="$(json_int "${status_json}" "indexed_items")"
  indexed_sources="$(json_int "${status_json}" "indexed_sources")"
  doctor_ok="$(json_bool "${doctor_json}" "ok")"
  validate_valid="$(json_bool "${validate_json}" "valid")"
  oracle_json="${tmp_root}/retrieval-oracle.json"
  json_provider_retrieval_oracle "${provider}" "${search_json}" > "${oracle_json}"
  oracle_pass="$(json_bool "${oracle_json}" "passed")"
  oracle_search_provider_matches="$(json_int "${oracle_json}" "search.provider_matches")"
  oracle_search_provider_mismatches="$(json_int "${oracle_json}" "search.provider_mismatches")"
  oracle_search_provider_missing="$(json_int "${oracle_json}" "search.provider_missing")"
  oracle_search_source_exists_true="$(json_int "${oracle_json}" "search.source_exists_true")"
  oracle_search_source_exists_false="$(json_int "${oracle_json}" "search.source_exists_false")"
  oracle_search_source_exists_missing="$(json_int "${oracle_json}" "search.source_exists_missing")"
  oracle_search_citation_count="$(json_int "${oracle_json}" "search.citation_count")"
  oracle_search_citation_provider_matches="$(json_int "${oracle_json}" "search.citation_provider_matches")"
  oracle_search_citation_provider_mismatches="$(json_int "${oracle_json}" "search.citation_provider_mismatches")"
  oracle_search_citation_provider_missing="$(json_int "${oracle_json}" "search.citation_provider_missing")"
  oracle_search_citation_source_exists_true="$(json_int "${oracle_json}" "search.citation_source_exists_true")"
  oracle_search_citation_source_exists_false="$(json_int "${oracle_json}" "search.citation_source_exists_false")"
  oracle_search_citation_source_exists_missing="$(json_int "${oracle_json}" "search.citation_source_exists_missing")"

  rm -rf "${tmp_root}"

  if (( search_results == 0 )); then
    write_live_failure_result "${provider}" "${out_dir}" "no_retrieval_results" \
      "ctx import completed but search returned no provider-filtered results"
    return 1
  fi
  if [[ "${oracle_pass}" != "true" ]]; then
    write_live_failure_result "${provider}" "${out_dir}" "retrieval_oracle_failed" \
      "ctx search did not return provider-filtered citations with source_exists=true"
    return 1
  fi
  if [[ "${doctor_ok}" != "true" || "${validate_valid}" != "true" ]]; then
    write_live_failure_result "${provider}" "${out_dir}" "ctx_health_failed" \
      "ctx doctor or validate reported an unhealthy temporary data root"
    return 1
  fi

  json="${out_dir}/live-e2e.json"
  markdown="${out_dir}/live-e2e.md"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"
  required_kind="$(ctx_path_kind "${required_path}")"
  optional_kind=""
  if [[ -n "${optional_path}" ]]; then
    optional_kind="$(ctx_path_kind "${optional_path}")"
  fi

  cat > "${json}" <<EOF
{
  "schema_version": 1,
  "kind": "provider_live_e2e_result",
  "publishing": false,
  "provider": "$(ctx_json_escape "${provider}")",
  "display_name": "$(ctx_json_escape "${display}")",
  "status": "passed",
  "evidence_class": "manual_opt_in_local_history",
  "provider_command_execution": false,
  "api_key_env_passed_to_ctx": false,
  "temporary_ctx_data_root": true,
  "raw_ctx_command_outputs_persisted": false,
  "raw_transcripts_persisted": false,
  "raw_snippets_persisted": false,
  "raw_queries_persisted": false,
  "raw_source_paths_persisted": false,
  "artifact_redaction": "aggregate_and_oracle_counts_only_no_raw_transcripts_snippets_queries_or_source_paths",
  "local_history_opt_in": true,
  "source_inputs": ${source_inputs},
  "source_paths_configured": ${source_paths_configured},
  "required_source_path_kind": "$(ctx_json_escape "${required_kind}")",
  "optional_source_configured": $(ctx_bool "${optional_source_configured}"),
  "optional_source_path_kind": "$(ctx_json_escape "${optional_kind}")",
  "query_configured": ${configured_query},
  "import": {
    "source_files": ${imported_source_files},
    "source_bytes": ${imported_source_bytes},
    "imported_sessions": ${imported_sessions},
    "imported_events": ${imported_events},
    "imported_edges": ${imported_edges},
    "skipped": ${import_skipped},
    "failed": ${import_failed}
  },
  "retrieval": {
    "search_results": ${search_results}
  },
  "retrieval_oracle": {
    "passed": ${oracle_pass},
    "query_basis": "configured_query",
    "expected_search_results_min": 1,
    "search": {
      "provider_matches": ${oracle_search_provider_matches},
      "provider_mismatches": ${oracle_search_provider_mismatches},
      "provider_missing": ${oracle_search_provider_missing},
      "source_exists_true": ${oracle_search_source_exists_true},
      "source_exists_false": ${oracle_search_source_exists_false},
      "source_exists_missing": ${oracle_search_source_exists_missing},
      "citation_count": ${oracle_search_citation_count},
      "citation_provider_matches": ${oracle_search_citation_provider_matches},
      "citation_provider_mismatches": ${oracle_search_citation_provider_mismatches},
      "citation_provider_missing": ${oracle_search_citation_provider_missing},
      "citation_source_exists_true": ${oracle_search_citation_source_exists_true},
      "citation_source_exists_false": ${oracle_search_citation_source_exists_false},
      "citation_source_exists_missing": ${oracle_search_citation_source_exists_missing}
    }
  },
  "health": {
    "indexed_items": ${indexed_items},
    "indexed_sources": ${indexed_sources},
    "doctor_ok": ${doctor_ok},
    "validate_valid": ${validate_valid}
  },
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at}
}
EOF

  cat > "${markdown}" <<EOF
# ${display} Live E2E

- Publishing: false
- Provider: \`${provider}\`
- Status: passed
- Evidence class: manual opt-in local history
- Provider command execution: false
- API key environment passed to ctx: false
- Temporary \`CTX_DATA_ROOT\`: true
- Artifact redaction: aggregate counts only; no raw transcripts, snippets, queries, or source paths.
- Raw ctx command outputs persisted: false
- Source inputs: ${source_inputs}
- Required source path kind: ${required_kind}
- Optional source configured: $(ctx_bool "${optional_source_configured}")
- Query configured: ${configured_query}
- Imported source files: ${imported_source_files}
- Imported sessions: ${imported_sessions}
- Imported events: ${imported_events}
- Imported edges: ${imported_edges}
- Search results: ${search_results}
- Retrieval oracle: ${oracle_pass}
- Retrieval oracle query basis: configured_query
- Search provider matches: ${oracle_search_provider_matches}
- Search provider mismatches: ${oracle_search_provider_mismatches}
- Search source exists true: ${oracle_search_source_exists_true}
- Search citation provider matches: ${oracle_search_citation_provider_matches}
- Search citation source exists true: ${oracle_search_citation_source_exists_true}
- Indexed items: ${indexed_items}
- Indexed sources: ${indexed_sources}
- Doctor OK: ${doctor_ok}
- Validate valid: ${validate_valid}
EOF

  artifact_guard_no_raw_values "${json}" "${markdown}" "${required_path}" "${optional_path}" "${raw_query_guard}"
}

run_selected() {
  local selected=0 passed=0 skipped=0 blocked=0 failed=0 provider env_name provider_dir status provider_exit

  mkdir -p "${CTX_ARTIFACT_DIR}"
  if [[ "${CTX_LIVE_PROVIDER_E2E:-0}" != "1" ]]; then
    write_selected_skip "${CTX_ARTIFACT_DIR}" "global_opt_in_missing" \
      "live provider E2E is opt-in; set CTX_LIVE_PROVIDER_E2E=1 and one CTX_LIVE_PROVIDER_<PROVIDER>=1 variable to run a provider lane"
    printf 'provider live E2E global opt-in is disabled\n'
    return 0
  fi

  while IFS= read -r provider; do
    env_name="$(provider_env_name "${provider}")"
    if [[ "${!env_name:-0}" == "1" ]]; then
      selected=$(( selected + 1 ))
      provider_dir="${CTX_ARTIFACT_DIR}/${provider}"
      set +e
      if [[ "$(provider_live_capability "${provider}")" == "local_history_smoke" ]]; then
        run_local_history_provider "${provider}" "${provider_dir}"
      else
        run_fixture_only_provider "${provider}" "${provider_dir}"
      fi
      provider_exit=$?
      set -e
      status="$(artifact_status "${provider_dir}/live-e2e.json")"
      status="${status:-unknown}"
      case "${status}" in
        passed) passed=$(( passed + 1 )) ;;
        skipped) skipped=$(( skipped + 1 )) ;;
        blocked) blocked=$(( blocked + 1 )) ;;
        *) failed=$(( failed + 1 )) ;;
      esac
      if (( provider_exit != 0 && status == "skipped" )); then
        failed=$(( failed + 1 ))
        skipped=$(( skipped - 1 ))
      fi
    fi
  done < <(provider_ids)

  if (( selected == 0 )); then
    write_selected_skip "${CTX_ARTIFACT_DIR}" "no_provider_selected" \
      "set one CTX_LIVE_PROVIDER_<PROVIDER>=1 variable to run a provider lane"
    printf 'provider live E2E selected no provider lanes\n'
    return 0
  fi

  write_selected_summary "${CTX_ARTIFACT_DIR}" "${selected}" "${passed}" "${skipped}" "${blocked}" "${failed}"
  if (( failed > 0 )); then
    return 1
  fi
  if (( blocked > 0 && "${CTX_LIVE_PROVIDER_E2E_ACCEPT_BLOCKERS:-0}" != "1" )); then
    return 1
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
      if [[ "$(provider_live_capability "${provider}")" == "local_history_smoke" ]]; then
        ctx_run_timed "provider-live-e2e-${provider}" run_local_history_provider "${provider}" "${CTX_ARTIFACT_DIR}"
      else
        ctx_run_timed "provider-live-e2e-${provider}" run_fixture_only_provider "${provider}" "${CTX_ARTIFACT_DIR}"
      fi
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
