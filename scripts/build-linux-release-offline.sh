#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/build-linux-release-offline.sh PLATFORM TARGET PREPARED_DIR TARGET_DIR

Builds a Linux release artifact from verified prepared inputs. The caller must
provide a network-disabled container with read-only source and prepared mounts.
USAGE
}

platform="${1:-}"
target="${2:-}"
prepared_dir="${3:-}"
target_dir="${4:-}"
if [[ -z "${platform}" || -z "${target}" || -z "${prepared_dir}" || -z "${target_dir}" ]]; then
  usage
  exit 2
fi

case "${platform}:${target}:$(uname -m)" in
  linux-x64:x86_64-unknown-linux-gnu:x86_64|linux-x64:x86_64-unknown-linux-gnu:amd64) ;;
  linux-aarch64:aarch64-unknown-linux-gnu:aarch64|linux-aarch64:aarch64-unknown-linux-gnu:arm64) ;;
  *)
    printf 'offline release build requires a matching native Linux host: %s %s %s\n' \
      "${platform}" "${target}" "$(uname -m)" >&2
    exit 1
    ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

python3 scripts/check-linux-release-network-isolation.py

if [[ ! -d "${prepared_dir}/cargo-home" ]]; then
  printf 'prepared Cargo inputs are missing: %s\n' "${prepared_dir}/cargo-home" >&2
  exit 1
fi
if [[ -e "${target_dir}" ]] && [[ -n "$(find "${target_dir}" -mindepth 1 -print -quit)" ]]; then
  printf 'offline release target must start empty: %s\n' "${target_dir}" >&2
  exit 1
fi

mkdir -p "${target_dir}"
final_cargo_home="${TMPDIR:-/tmp}/ctx-release-cargo-home"
rm -rf "${final_cargo_home}"
mkdir -p "${final_cargo_home}"
cp -a "${prepared_dir}/cargo-home/." "${final_cargo_home}/"
chmod -R u+rwX "${final_cargo_home}"

export CARGO_HOME="${final_cargo_home}"
export CARGO_TARGET_DIR="${target_dir}"
export CARGO_NET_OFFLINE=true
unset CARGO_BUILD_TARGET CARGO_ENCODED_RUSTFLAGS RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER

if ! rustup target list --installed | grep -Fx "${target}" >/dev/null; then
  printf 'release builder does not contain required Rust target: %s\n' "${target}" >&2
  exit 1
fi

if [[ "${platform}" == "linux-x64" ]]; then
  export RUSTFLAGS="-C target-cpu=x86-64"
else
  unset RUSTFLAGS
fi

cargo build -p ctx --release --target "${target}" --locked --offline
printf 'built Linux release offline: %s %s\n' "${platform}" "${target}"
