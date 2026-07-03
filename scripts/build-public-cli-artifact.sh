#!/usr/bin/env bash
set -euo pipefail

ZIG_VERSION="0.14.1"
ZIG_LINUX_X64_URL="https://ziglang.org/download/${ZIG_VERSION}/zig-x86_64-linux-${ZIG_VERSION}.tar.xz"
ZIG_LINUX_X64_SHA256="24aeeec8af16c381934a6cd7d95c807a8cb2cf7df9fa40d359aa884195c4716c"
CARGO_ZIGBUILD_VERSION="0.23.0"

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

ensure_zig_for_linux_x64() {
  if command -v zig >/dev/null 2>&1; then
    return
  fi

  if [[ "$(uname -s)" != "Linux" ]]; then
    echo "error: zig is required to cross-build ${platform} from $(uname -s)" >&2
    exit 127
  fi
  case "$(uname -m)" in
    x86_64|amd64) ;;
    *)
      echo "error: automatic Zig bootstrap only supports Linux x86_64, got $(uname -m)" >&2
      exit 127
      ;;
  esac

  for required_tool in curl tar; do
    if ! command -v "${required_tool}" >/dev/null 2>&1; then
      echo "error: ${required_tool} is required to bootstrap Zig ${ZIG_VERSION}" >&2
      exit 127
    fi
  done

  toolchain_dir="${CTX_PUBLIC_CLI_TOOLCHAIN_DIR:-target/public-cli-toolchain}"
  install_dir="${toolchain_dir}/zig-x86_64-linux-${ZIG_VERSION}"
  if [[ ! -x "${install_dir}/zig" ]]; then
    mkdir -p "${toolchain_dir}"
    archive="${toolchain_dir}/zig-x86_64-linux-${ZIG_VERSION}.tar.xz"
    tmp_archive="${archive}.tmp"
    curl -fsSL "${ZIG_LINUX_X64_URL}" -o "${tmp_archive}"
    if command -v sha256sum >/dev/null 2>&1; then
      actual_sha="$(sha256sum "${tmp_archive}" | awk '{ print $1 }')"
    elif command -v shasum >/dev/null 2>&1; then
      actual_sha="$(shasum -a 256 "${tmp_archive}" | awk '{ print $1 }')"
    else
      echo "error: sha256sum or shasum is required to verify Zig ${ZIG_VERSION}" >&2
      exit 127
    fi
    if [[ "${actual_sha}" != "${ZIG_LINUX_X64_SHA256}" ]]; then
      echo "error: Zig ${ZIG_VERSION} checksum mismatch: expected ${ZIG_LINUX_X64_SHA256}, got ${actual_sha}" >&2
      exit 1
    fi
    mv "${tmp_archive}" "${archive}"
    rm -rf "${install_dir}"
    tar -C "${toolchain_dir}" -xf "${archive}"
  fi
  export PATH="${install_dir}:${PATH}"
}

ensure_darwin_cross_tools() {
  if ! command -v cargo-zigbuild >/dev/null 2>&1; then
    cargo install cargo-zigbuild --version "${CARGO_ZIGBUILD_VERSION}" --locked
  fi
  ensure_zig_for_linux_x64
  command -v zig >/dev/null 2>&1 || {
    echo "error: zig is required to cross-build ${platform} from $(uname -s)" >&2
    exit 127
  }
}

version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; data=json.load(sys.stdin); print(next(pkg["version"] for pkg in data["packages"] if pkg["name"] == "ctx"))')"
if [[ -z "${version}" ]]; then
  echo "error: could not determine ctx package version from Cargo metadata" >&2
  exit 1
fi
echo "building ctx ${version} for ${platform}"

rustup target add "${target}" >/dev/null
out_dir="${CTX_PUBLIC_CLI_ARTIFACT_DIR:-target/public-cli-artifacts}"
mkdir -p "${out_dir}"

if [[ "${platform}" == macos-* && "$(uname -s)" != "Darwin" ]]; then
  ensure_darwin_cross_tools
  cargo zigbuild -p ctx --release --target "${target}" --locked
elif [[ "${platform}" == "freebsd-x64" ]]; then
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
    grep -Fx "ctx ${version}" "${staged}.version" >/dev/null
    ;;
  macos-arm64)
    if [[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "arm64" ]]; then
      "${staged}" --version | tee "${staged}.version"
      grep -Fx "ctx ${version}" "${staged}.version" >/dev/null
    else
      printf 'not run on this host: %s\n' "${platform}" > "${staged}.version"
    fi
    ;;
  macos-x64)
    if [[ "$(uname -s)" == "Darwin" ]] && /usr/bin/arch -x86_64 /usr/bin/true >/dev/null 2>&1; then
      /usr/bin/arch -x86_64 "${staged}" --version | tee "${staged}.version"
      grep -Fx "ctx ${version}" "${staged}.version" >/dev/null
    else
      printf 'not run on this host: %s\n' "${platform}" > "${staged}.version"
    fi
    ;;
  *)
    printf 'not run on this host: %s\n' "${platform}" > "${staged}.version"
    ;;
esac

scripts/check-public-cli-artifact.sh "${platform}" "${out_dir}"

printf 'built %s for %s sha256=%s\n' "${staged}" "${platform}" "$(cat "${sha_file}")"
