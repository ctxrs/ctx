#!/usr/bin/env bash
set -euo pipefail

# The shared macOS Buildkite agent invokes this repo-owned hook before each job.
# Public ctx CLI jobs do not need the private desktop release host checks.
exit 0
