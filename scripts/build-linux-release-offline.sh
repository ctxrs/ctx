#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/build-linux-release-offline.sh PLATFORM TARGET

Internal container entrypoint. Builds and stages one Linux release artifact
using the fixed /work, /prepared, /release-target, and /artifacts mounts.
USAGE
}

platform="${1:-}"
target="${2:-}"
if [[ $# -ne 2 || -z "${platform}" || -z "${target}" ]]; then
  usage
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "${repo_root}"
bash scripts/check-linux-release-environment.sh
[[ "${repo_root}" == "/work" ]] || {
  echo "offline Linux release entrypoint requires the fixed /work container mount" >&2
  exit 1
}

case "${platform}:${target}:$(/usr/bin/uname -m)" in
  linux-x64:x86_64-unknown-linux-gnu:x86_64|\
  linux-x64:x86_64-unknown-linux-gnu:amd64) ;;
  linux-aarch64:aarch64-unknown-linux-gnu:aarch64|\
  linux-aarch64:aarch64-unknown-linux-gnu:arm64) ;;
  *)
    printf 'offline release build requires a matching native Linux host: %s %s %s\n' \
      "${platform}" "${target}" "$(/usr/bin/uname -m)" >&2
    exit 1
    ;;
esac

readonly prepared_dir="/prepared"
readonly target_dir="/release-target"
readonly artifact_dir="/artifacts"
for path in "${prepared_dir}" "${target_dir}" "${artifact_dir}"; do
  [[ -d "${path}" && ! -L "${path}" ]] || {
    printf 'required Linux release mount is not a real directory: %s\n' \
      "${path}" >&2
    exit 1
  }
done

bash scripts/check-linux-release-builder.sh "${target}"
/usr/bin/python3 -I scripts/check-linux-release-network-isolation.py

if [[ ! -d "${prepared_dir}/cargo-home" || -L "${prepared_dir}/cargo-home" ]]; then
  printf 'prepared Cargo inputs are missing: %s\n' "${prepared_dir}/cargo-home" >&2
  exit 1
fi
if [[ -n "$(/usr/bin/find "${target_dir}" -mindepth 1 -print -quit)" ]]; then
  printf 'offline release target must start empty: %s\n' "${target_dir}" >&2
  exit 1
fi

final_cargo_home="/tmp/ctx-release-cargo-home"
/usr/bin/rm -rf "${final_cargo_home}"
/usr/bin/mkdir -p "${final_cargo_home}"
/usr/bin/cp -a "${prepared_dir}/cargo-home/." "${final_cargo_home}/"
/usr/bin/chmod -R u+rwX "${final_cargo_home}"

export PATH="/opt/cargo/bin:/usr/bin:/bin"
export RUSTUP_HOME="/opt/rustup"
export CARGO_HOME="${final_cargo_home}"
export CARGO_TARGET_DIR="${target_dir}"
export CARGO_NET_OFFLINE=true
unset \
  CARGO_BUILD_TARGET CARGO_ENCODED_RUSTFLAGS RUSTC_WRAPPER \
  RUSTC_WORKSPACE_WRAPPER

if ! /opt/cargo/bin/rustup target list --installed \
  | /usr/bin/grep -Fx "${target}" >/dev/null; then
  printf 'release builder does not contain required Rust target: %s\n' "${target}" >&2
  exit 1
fi

if [[ "${platform}" == "linux-x64" ]]; then
  export RUSTFLAGS="-C target-cpu=x86-64"
  binary_name="ctx"
else
  unset RUSTFLAGS
  binary_name="ctx-linux-aarch64"
fi

version="$(/opt/cargo/bin/cargo metadata --no-deps --format-version 1 \
  --locked --offline \
  | /usr/bin/python3 -I -c \
    'import json,sys; data=json.load(sys.stdin); print(next(pkg["version"] for pkg in data["packages"] if pkg["name"] == "ctx"))')"
[[ -n "${version}" ]] || {
  echo "could not determine ctx package version from offline Cargo metadata" >&2
  exit 1
}

/opt/cargo/bin/cargo build -p ctx --release --target "${target}" --locked --offline
target_binary="${target_dir}/${target}/release/ctx"
[[ -f "${target_binary}" && ! -L "${target_binary}" ]] || {
  printf 'Linux release compiler did not produce the expected binary: %s\n' \
    "${target_binary}" >&2
  exit 1
}

staged="${artifact_dir}/${binary_name}"
/usr/bin/cp "${target_binary}" "${staged}"
/usr/bin/chmod 0755 "${staged}"
/usr/bin/sha256sum "${staged}" | /usr/bin/awk '{ print $1 }' >"${staged}.sha256"
"${staged}" --version | /usr/bin/tee "${staged}.version"
/usr/bin/grep -Fx "ctx ${version}" "${staged}.version" >/dev/null

printf 'built and staged Linux release offline: %s %s\n' \
  "${platform}" "${target}"
