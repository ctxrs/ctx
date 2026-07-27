#!/usr/bin/env bash
set -euo pipefail

if (( "$#" > 1 )); then
  printf 'usage: %s [strings-file]\n' "$(basename "$0")" >&2
  exit 2
fi

input="${1:-/dev/stdin}"
if [[ ! -f "${input}" && "${input}" != "/dev/stdin" ]]; then
  printf 'release binary strings file not found: %s\n' "${input}" >&2
  exit 2
fi

# These are exact names from the retired hosted-history client. Keep this list
# scoped to its packages, routes, config, queue/publication types, and dashboard
# entry points: generic Protocol V1 kinds such as `pull_request` are public wire
# vocabulary and must remain valid release strings.
removed_cloud_history_pattern='ctx[-_]cloud[-_]client|ctx[-_]captured[-_]batch'
removed_cloud_history_pattern+='|Cloud(PublicationSink|Uploader|CredentialLocator|Endpoint|CaptureRuntime|RuntimeSyncReport)'
removed_cloud_history_pattern+='|Publication(WindowInput|RecoveryReport|CursorRecoveryOutcome)'
removed_cloud_history_pattern+='|CTX_CLOUD_(API_BASE|MODE|TOKEN|CREDENTIAL_(FD|FILE)|TRUST_BUNDLE_(FILE|SHA))'
removed_cloud_history_pattern+='|v1/(sources/resolve|uploads/reservations)'
removed_cloud_history_pattern+='|v1/uploads/[^[:space:]]+/finalize|v1/imports/[^[:space:]]+/seal'
removed_cloud_history_pattern+='|local_revision_publication_windows|spool_batches_ready|cloud_sync_(status|reason)'
removed_cloud_history_pattern+='|ctx[-_]dashboard|dashboard_url|open_dashboard_url|running_dashboard_url'
removed_cloud_history_pattern+='|work_record_core[^[:space:]]*PublishedTo|work[-_]record[-_](publish|report|vcs)'

if LC_ALL=C grep -a -E "${removed_cloud_history_pattern}" "${input}" >/dev/null; then
  printf 'release binary contains a removed hosted-history runtime signature\n' >&2
  exit 1
fi
