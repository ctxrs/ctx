#!/usr/bin/env bash
set -euo pipefail

if (( "$#" != 1 )); then
  printf 'usage: %s TEST_BINARY\n' "$0" >&2
  exit 2
fi

test_binary="$1"
[[ -x "$test_binary" ]] || {
  printf 'source-backed recovery test binary is not executable: %s\n' "$test_binary" >&2
  exit 2
}
: "${TEST_TMPDIR:?Bazel must provide TEST_TMPDIR}"

test_binary="$(cd -- "$(dirname -- "$test_binary")" && pwd -P)/$(basename -- "$test_binary")"
mountpoint="${TEST_TMPDIR}/bounded-enospc"
mkdir -- "$mountpoint"
trap 'rmdir -- "$mountpoint"' EXIT

sudo -n unshare --mount --propagation private sh -eu -c '
  mount -t tmpfs -o size=128m,nr_inodes=16384 tmpfs "$1"
  trap '"'"'umount -- "$1"'"'"' EXIT
  chmod 0777 "$1"
  findmnt -no TARGET,FSTYPE,OPTIONS "$1"
  df -B1 "$1"
  TMPDIR="$1" \
  CTX_SOURCE_RECOVERY_REAL_ENOSPC=1 \
    "$2" \
      --exact actual_bounded_filesystem_enospc_preserves_previous_generation \
      --ignored \
      --nocapture \
      --test-threads=1
' sh "$mountpoint" "$test_binary"
