#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
archive=""
evidence_out=""
public_ctx_repo="${CTX_PUBLIC_CTX_REPO:-}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --archive) shift; archive="${1:-}" ;;
    --evidence-out) shift; evidence_out="${1:-}" ;;
    --public-ctx-repo) shift; public_ctx_repo="${1:-}" ;;
    -h|--help)
      printf 'usage: repair-coreml-semantic-object.sh --archive PATH --evidence-out PATH --public-ctx-repo PATH\n' >&2
      exit 0
      ;;
    *) printf 'error: unknown argument: %s\n' "$1" >&2; exit 64 ;;
  esac
  shift
done
[[ -n "$archive" && -f "$archive" && ! -L "$archive" ]] || { echo 'error: --archive is required' >&2; exit 64; }
[[ -n "$evidence_out" && ! -e "$evidence_out" ]] || { echo 'error: --evidence-out must not exist' >&2; exit 64; }
[[ -n "$public_ctx_repo" && -d "$public_ctx_repo" && ! -L "$public_ctx_repo" ]] || { echo 'error: --public-ctx-repo is required' >&2; exit 64; }

# Capture and execute the complete validator closure before all credential
# access. The preflight durably reserves the evidence record and leaves an
# identity-bound handoff for the publisher; it cannot inherit signing, R2, or
# Infisical state.
env -i PATH="$PATH" \
  node "$root/scripts/release/repair-coreml-semantic-object.mjs" preflight \
    --archive "$archive" --evidence-out "$evidence_out" --public-ctx-repo "$public_ctx_repo"

# Re-open the artifact and handoff while this process remains credential-free,
# then durably record that the next action is the first credential lookup.
env -i PATH="$PATH" \
  node "$root/scripts/release/repair-coreml-semantic-object.mjs" begin-credentials \
    --archive "$archive" --evidence-out "$evidence_out"

credential_failed() {
  local failure="$1"
  if ! env -i PATH="$PATH" \
    node "$root/scripts/release/repair-coreml-semantic-object.mjs" credential-failed \
      --evidence-out "$evidence_out" --failure "$failure"; then
    echo 'error: CoreML repair could not record its credential failure' >&2
  fi
}

secret() {
  infisical secrets get "$1" --env prod --path / --plain --silent \
    --expand=false --include-imports=false --secret-overriding=false
}
trap 'unset r2_access r2_secret r2_endpoint r2_bucket' EXIT
if ! r2_access="$(secret CTX_RELEASE_R2_ACCESS_KEY_ID)"; then
  credential_failed access-key-fetch-failed
  exit 1
fi
if ! r2_secret="$(secret CTX_RELEASE_R2_SECRET_ACCESS_KEY)"; then
  credential_failed secret-key-fetch-failed
  exit 1
fi
if ! r2_endpoint="$(secret CTX_RELEASE_R2_ENDPOINT)"; then
  credential_failed endpoint-fetch-failed
  exit 1
fi
if ! r2_bucket="$(secret CTX_RELEASE_R2_BUCKET)"; then
  credential_failed bucket-fetch-failed
  exit 1
fi
if [[ "$r2_bucket" != ctx-releases-prod ]]; then
  credential_failed bucket-mismatch
  echo 'error: CoreML semantic object repair bucket differs from fixed authority' >&2
  exit 1
fi

# The publisher sees only the four production R2 values. It receives no public
# checkout path and executes no public bytes; it revalidates only the pinned
# artifact and the private preflight handoff.
env -i PATH="$PATH" \
  CTX_RELEASE_R2_ACCESS_KEY_ID="$r2_access" \
  CTX_RELEASE_R2_SECRET_ACCESS_KEY="$r2_secret" \
  CTX_RELEASE_R2_ENDPOINT="$r2_endpoint" \
  CTX_RELEASE_R2_BUCKET="$r2_bucket" \
  node "$root/scripts/release/repair-coreml-semantic-object.mjs" publish \
    --archive "$archive" --evidence-out "$evidence_out"
printf 'CoreML semantic object repair evidence: %s\n' "$evidence_out"
