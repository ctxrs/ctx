#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(dirname "${SCRIPT_DIR}")"
CORE_ROOT="${REPO_ROOT}/core"
TARGET="x86_64-pc-windows-gnu"
OUT_DIR="${REPO_ROOT}/ctx-cli-windows-x64"
RUST_TOOLCHAIN="${CTX_WINDOWS_RUST_TOOLCHAIN:-1.95.0}"

require_command() {
  local command_name="$1"
  if ! command -v "${command_name}" >/dev/null 2>&1; then
    echo "error: required command '${command_name}' is missing from PATH" >&2
    exit 127
  fi
}

verify_rustup() {
  export PATH="${HOME}/.cargo/bin:${PATH}"
  if ! command -v rustup >/dev/null 2>&1; then
    echo "error: rustup is required on the Windows CLI cross-build runner; install rustup in the runner image instead of bootstrapping it during release verification" >&2
    exit 2
  fi
}

verify_mingw() {
  if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
    echo "error: x86_64-w64-mingw32-gcc is required on the Windows CLI cross-build runner; install gcc-mingw-w64-x86-64 in the runner image" >&2
    exit 2
  fi
  x86_64-w64-mingw32-gcc -dumpfullversion -dumpversion
}

verify_rustup
verify_mingw

rustup toolchain install "${RUST_TOOLCHAIN}" --profile minimal --target "${TARGET}"
export RUSTUP_TOOLCHAIN="${RUST_TOOLCHAIN}"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="${CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER:-x86_64-w64-mingw32-gcc}"
export RUSTFLAGS="${RUSTFLAGS:--C target-feature=+crt-static}"

rm -rf "${OUT_DIR}"
mkdir -p "${OUT_DIR}"

cargo build \
  --manifest-path "${CORE_ROOT}/Cargo.toml" \
  -p ctx-http \
  --bin ctx \
  --locked \
  --release \
  --target "${TARGET}"

cp "${CORE_ROOT}/target/${TARGET}/release/ctx.exe" "${OUT_DIR}/ctx.exe"
sha256sum "${OUT_DIR}/ctx.exe" > "${OUT_DIR}/ctx.exe.sha256"
test -s "${OUT_DIR}/ctx.exe"
