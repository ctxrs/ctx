#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
source_commit="0000000000000000000000000000000000000000"
if git -C "${repo_root}" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  source_commit="$(git -C "${repo_root}" rev-parse --verify HEAD^{commit})"
fi

[[ "${source_commit}" =~ ^[0-9a-f]{40}$ ]] || {
  printf 'error: workspace source commit is invalid\n' >&2
  exit 1
}

cargo_lock_sha256="$(python3 - "${repo_root}/Cargo.lock" <<'PY'
import hashlib
import sys
from pathlib import Path

path = Path(sys.argv[1])
if not path.is_file():
    raise SystemExit("Cargo.lock is unavailable")
print(hashlib.sha256(path.read_bytes()).hexdigest())
PY
)"
[[ "${cargo_lock_sha256}" =~ ^[0-9a-f]{64}$ ]] || {
  printf 'error: workspace Cargo.lock digest is invalid\n' >&2
  exit 1
}

printf 'STABLE_CTX_SOURCE_COMMIT %s\n' "${source_commit}"
printf 'STABLE_CTX_CARGO_LOCK_SHA256 %s\n' "${cargo_lock_sha256}"
