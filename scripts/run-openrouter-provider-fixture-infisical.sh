#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
usage: scripts/run-openrouter-provider-fixture-infisical.sh COMMAND [ARG...]

Runs a manual OpenRouter fixture-drafting command with secrets hydrated through Infisical.
If the runner hook already provided OPENROUTER_API_KEY or CTX_OPENROUTER_API_KEY,
the command is executed with that pre-hydrated environment.

Configuration:
  CTX_OPENROUTER_FIXTURE_USE_INFISICAL=0  bypass Infisical and exec COMMAND
  CTX_OPENROUTER_INFISICAL_PROJECT_ID           Infisical project id
  CTX_OPENROUTER_INFISICAL_ENV                  Infisical environment, defaults to prod
  CTX_OPENROUTER_INFISICAL_PATH                 Infisical path, defaults to /
  CTX_OPENROUTER_INFISICAL_DOMAIN               Optional Infisical API endpoint

Fallback generic names are also accepted:
  CTX_INFISICAL_PROJECT_ID
  CTX_INFISICAL_ENV
  CTX_INFISICAL_PATH
  CTX_INFISICAL_DOMAIN
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if (( $# == 0 )); then
  usage >&2
  exit 64
fi

if [[ "${CTX_OPENROUTER_FIXTURE_USE_INFISICAL:-1}" != "1" ]]; then
  exec "$@"
fi

project_id="${CTX_OPENROUTER_INFISICAL_PROJECT_ID:-${CTX_INFISICAL_PROJECT_ID:-}}"
env_name="${CTX_OPENROUTER_INFISICAL_ENV:-${CTX_INFISICAL_ENV:-prod}}"
secret_path="${CTX_OPENROUTER_INFISICAL_PATH:-${CTX_INFISICAL_PATH:-/}}"
domain="${CTX_OPENROUTER_INFISICAL_DOMAIN:-${CTX_INFISICAL_DOMAIN:-}}"

if ! command -v infisical >/dev/null 2>&1; then
  if [[ -n "${OPENROUTER_API_KEY:-${CTX_OPENROUTER_API_KEY:-}}" ]]; then
    printf 'infisical CLI unavailable; using pre-hydrated OpenRouter environment\n' >&2
    exec "$@"
  fi
  printf 'infisical CLI is required when OpenRouter credential env is not pre-hydrated\n' >&2
  exit 127
fi

if [[ -z "${project_id}" ]]; then
  printf 'CTX_OPENROUTER_INFISICAL_PROJECT_ID or CTX_INFISICAL_PROJECT_ID is required\n' >&2
  exit 64
fi

infisical_cmd=(infisical)
if [[ -n "${domain}" ]]; then
  infisical_cmd+=(--domain "${domain}")
fi
infisical_cmd+=(run --env="${env_name}" --path="${secret_path}" --projectId="${project_id}" --)

exec "${infisical_cmd[@]}" "$@"
