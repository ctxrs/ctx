#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

publication=""
runtime_handoff=""
semantic_artifact_dir=""
candidate_manifest_handoff=""
candidate_handoff_sha256=""
public_ctx_repo="${CTX_PUBLIC_CTX_REPO:-$root}"
work_dir=""
published_at=""
preflight_only=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --publication) shift; publication="${1:-}" ;;
    --runtime-handoff) shift; runtime_handoff="${1:-}" ;;
    --semantic-artifact-dir) shift; semantic_artifact_dir="${1:-}" ;;
    --candidate-manifest-handoff) shift; candidate_manifest_handoff="${1:-}" ;;
    --candidate-handoff-sha256) shift; candidate_handoff_sha256="${1:-}" ;;
    --public-ctx-repo) shift; public_ctx_repo="${1:-}" ;;
    --work-dir) shift; work_dir="${1:-}" ;;
    --published-at) shift; published_at="${1:-}" ;;
    --preflight-only) preflight_only=1 ;;
    *) echo "error: unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done
[[ -n "$publication" && -f "$publication" ]] || { echo "error: --publication is required" >&2; exit 2; }
[[ -n "$runtime_handoff" && -f "$runtime_handoff" && ! -L "$runtime_handoff" ]] || { echo "error: --runtime-handoff is required" >&2; exit 2; }
[[ -n "$semantic_artifact_dir" && -d "$semantic_artifact_dir" && ! -L "$semantic_artifact_dir" ]] || { echo "error: --semantic-artifact-dir is required" >&2; exit 2; }
[[ -n "$candidate_manifest_handoff" && -d "$candidate_manifest_handoff" && ! -L "$candidate_manifest_handoff" ]] || { echo "error: --candidate-manifest-handoff is required" >&2; exit 2; }
[[ "$candidate_handoff_sha256" =~ ^[0-9a-f]{64}$ && ! "$candidate_handoff_sha256" =~ ^0{64}$ ]] || { echo "error: --candidate-handoff-sha256 must be a nonzero lowercase SHA-256 digest" >&2; exit 2; }
[[ -n "$public_ctx_repo" && -d "$public_ctx_repo" && ! -L "$public_ctx_repo" ]] || { echo "error: --public-ctx-repo is required" >&2; exit 2; }
[[ -n "$work_dir" && ! -e "$work_dir" ]] || { echo "error: --work-dir must not exist" >&2; exit 2; }
[[ -n "$published_at" ]] || { echo "error: --published-at is required for retry-stable metadata" >&2; exit 2; }
mkdir -m 0700 "$work_dir"

# prepare validates the current release and five runtime transports before secret().
metadata="$work_dir/ctx-release-metadata.env"
signature="$metadata.sig"
evidence="$work_dir/publication-evidence.json"
prepare_args=(
  node scripts/release/publish-hosted-managed-pair-stable.mjs prepare
  --publication "$publication"
  --public-ctx-repo "$public_ctx_repo"
  --metadata-out "$metadata"
  --published-at "$published_at"
)
prepare_args+=(
  --runtime-handoff "$runtime_handoff"
  --candidate-handoff-sha256 "$candidate_handoff_sha256"
  --semantic-artifact-dir "$semantic_artifact_dir"
  --candidate-manifest-handoff "$candidate_manifest_handoff"
)
"${prepare_args[@]}"

if [[ "$preflight_only" == 1 ]]; then
  printf 'hosted stable secret-free preflight metadata: %s\n' "$metadata"
  exit 0
fi

secret() {
  if [[ -n "${!1:-}" ]]; then
    printf '%s' "${!1}"
  else
    infisical secrets get "$1" --env prod --path / --plain --silent
  fi
}
metadata_key="$(secret CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM)"
sign_args=(
  scripts/release/ctx_cli_release_metadata_sign.sh
  --metadata "$metadata"
  --out "$signature"
  --managed-pair-publication "$publication"
  --public-ctx-repo "$public_ctx_repo"
)
sign_args+=(
  --runtime-handoff "$runtime_handoff"
  --candidate-handoff-sha256 "$candidate_handoff_sha256"
  --semantic-artifact-dir "$semantic_artifact_dir"
  --candidate-manifest-handoff "$candidate_manifest_handoff"
)
env -i PATH="$PATH" HOME="$HOME" \
  CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="$metadata_key" \
  "${sign_args[@]}"
unset metadata_key

r2_access="$(secret CTX_RELEASE_R2_ACCESS_KEY_ID)"
r2_secret="$(secret CTX_RELEASE_R2_SECRET_ACCESS_KEY)"
r2_endpoint="$(secret CTX_RELEASE_R2_ENDPOINT)"
r2_bucket="$(secret CTX_RELEASE_R2_BUCKET)"
publish_args=(
  node scripts/release/publish-hosted-managed-pair-stable.mjs publish
  --publication "$publication"
  --public-ctx-repo "$public_ctx_repo"
  --metadata "$metadata"
  --signature "$signature"
  --evidence-out "$evidence"
)
publish_args+=(
  --runtime-handoff "$runtime_handoff"
  --candidate-handoff-sha256 "$candidate_handoff_sha256"
  --semantic-artifact-dir "$semantic_artifact_dir"
  --candidate-manifest-handoff "$candidate_manifest_handoff"
)
env -i PATH="$PATH" HOME="$HOME" \
  CTX_RELEASE_R2_ACCESS_KEY_ID="$r2_access" \
  CTX_RELEASE_R2_SECRET_ACCESS_KEY="$r2_secret" \
  CTX_RELEASE_R2_ENDPOINT="$r2_endpoint" \
  CTX_RELEASE_R2_BUCKET="$r2_bucket" \
  "${publish_args[@]}"
unset r2_access r2_secret r2_endpoint r2_bucket
printf 'hosted stable publication evidence: %s\n' "$evidence"
