#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/build-public-cli-artifact.sh PLATFORM

Builds one public ctx CLI binary and stages it under target/public-cli-artifacts.
Platforms: linux-x64, macos-arm64, macos-x64, windows-x64, freebsd-x64.
USAGE
}

platform="${1:-}"
if [[ -z "${platform}" || "${platform}" == "-h" || "${platform}" == "--help" ]]; then
  usage
  exit 2
fi

case "${platform}" in
  linux-x64)
    target="x86_64-unknown-linux-gnu"
    binary_name="ctx"
    ;;
  macos-arm64)
    target="aarch64-apple-darwin"
    binary_name="ctx-macos-arm64"
    ;;
  macos-x64)
    target="x86_64-apple-darwin"
    binary_name="ctx-macos-x64"
    ;;
  windows-x64)
    target="x86_64-pc-windows-gnu"
    binary_name="ctx.exe"
    ;;
  freebsd-x64)
    target="x86_64-unknown-freebsd"
    binary_name="ctx-freebsd-x64"
    ;;
  *)
    usage
    exit 2
    ;;
esac

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${root_dir}"

version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; data=json.load(sys.stdin); print(next(pkg["version"] for pkg in data["packages"] if pkg["name"] == "ctx"))')"
if [[ "${version}" != "0.4.0" ]]; then
  echo "error: ctx package version must be 0.4.0 for this release, got ${version}" >&2
  exit 1
fi

rustup target add "${target}" >/dev/null
out_dir="${CTX_PUBLIC_CLI_ARTIFACT_DIR:-target/public-cli-artifacts}"
mkdir -p "${out_dir}"

if [[ "${platform}" == "freebsd-x64" ]]; then
  if ! command -v cross >/dev/null 2>&1; then
    cargo install cross --locked
  fi
  cross build -p ctx --release --target "${target}" --locked
else
  cargo build -p ctx --release --target "${target}" --locked
fi

target_binary="target/${target}/release/ctx"
if [[ "${platform}" == "windows-x64" ]]; then
  target_binary="${target_binary}.exe"
fi
staged="${out_dir}/${binary_name}"
cp "${target_binary}" "${staged}"
chmod 755 "${staged}"

if command -v file >/dev/null 2>&1; then
  file "${staged}"
fi

sha_file="${staged}.sha256"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "${staged}" | awk '{ print $1 }' > "${sha_file}"
else
  shasum -a 256 "${staged}" | awk '{ print $1 }' > "${sha_file}"
fi

case "${platform}" in
  linux-x64)
    "${staged}" --version | tee "${staged}.version"
    grep -Fx "ctx 0.4.0" "${staged}.version" >/dev/null
    ;;
  macos-arm64)
    if [[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "arm64" ]]; then
      "${staged}" --version | tee "${staged}.version"
      grep -Fx "ctx 0.4.0" "${staged}.version" >/dev/null
    else
      printf 'not run on this host: %s\n' "${platform}" > "${staged}.version"
    fi
    ;;
  macos-x64)
    if [[ "$(uname -s)" == "Darwin" ]] && /usr/bin/arch -x86_64 /usr/bin/true >/dev/null 2>&1; then
      /usr/bin/arch -x86_64 "${staged}" --version | tee "${staged}.version"
      grep -Fx "ctx 0.4.0" "${staged}.version" >/dev/null
    else
      printf 'not run on this host: %s\n' "${platform}" > "${staged}.version"
    fi
    ;;
  *)
    printf 'not run on this host: %s\n' "${platform}" > "${staged}.version"
    ;;
esac

printf 'built %s for %s sha256=%s\n' "${staged}" "${platform}" "$(cat "${sha_file}")"
