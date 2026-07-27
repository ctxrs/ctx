#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf 'Linux release builder contract failed: %s\n' "$1" >&2
  exit 1
}

usage() {
  echo "usage: scripts/check-linux-release-builder.sh RUST_TARGET" >&2
  exit 2
}

target="${1:-}"
[[ $# -eq 1 ]] || usage
case "${target}" in
  x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu) ;;
  *) usage ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "${repo_root}"
bash scripts/check-linux-release-environment.sh

readonly os_release="/etc/os-release"
readonly getconf_tool="/usr/bin/getconf"
readonly rustc_tool="/opt/cargo/bin/rustc"
readonly cargo_tool="/opt/cargo/bin/cargo"
readonly python_tool="/usr/bin/python3"

[[ -r "${os_release}" && -f "${os_release}" ]] \
  || fail "OS release metadata must be a readable regular file"
for tool in \
  "${getconf_tool}" "${rustc_tool}" "${cargo_tool}" "${python_tool}" \
  /usr/bin/awk /usr/bin/sed; do
  [[ -x "${tool}" ]] || fail "required production tool is not executable: ${tool}"
done

os_value() {
  local key="$1"
  local value
  value="$(/usr/bin/awk -F= -v key="${key}" '
    $1 == key {
      count++
      value = substr($0, index($0, "=") + 1)
    }
    END {
      if (count != 1) exit 1
      print value
    }
  ' "${os_release}")" || fail "OS release metadata must contain exactly one ${key}"
  value="${value#\"}"
  value="${value%\"}"
  [[ -n "${value}" && "${value}" != *\"* ]] || fail "OS release ${key} is malformed"
  printf '%s\n' "${value}"
}

actual_os_id="$(os_value ID)"
actual_os_version="$(os_value VERSION_ID)"
actual_glibc="$("${getconf_tool}" GNU_LIBC_VERSION)" \
  || fail "getconf could not report GNU libc"

rustc_verbose="$("${rustc_tool}" -Vv)" || fail "rustc version inspection failed"
unique_rust_field() {
  local field="$1"
  local value
  value="$(/usr/bin/sed -n "s/^${field}: //p" <<<"${rustc_verbose}")"
  [[ -n "${value}" && "${value}" != *$'\n'* ]] \
    || fail "rustc -Vv must contain exactly one ${field}"
  printf '%s\n' "${value}"
}
actual_rust="$(unique_rust_field release)"
actual_commit="$(unique_rust_field commit-hash)"
actual_host="$(unique_rust_field host)"
actual_cargo="$("${cargo_tool}" --version)" \
  || fail "cargo version inspection failed"
actual_sysroot="$("${rustc_tool}" --print sysroot)" \
  || fail "rustc sysroot inspection failed"
actual_target_libdir="$("${rustc_tool}" --print target-libdir --target "${target}")" \
  || fail "Rust target sysroot inspection failed"

"${python_tool}" -I scripts/validate-linux-release-builder.py \
  --target "${target}" \
  --os-id "${actual_os_id}" \
  --os-version "${actual_os_version}" \
  --glibc "${actual_glibc}" \
  --rust-release "${actual_rust}" \
  --rust-commit "${actual_commit}" \
  --rust-host "${actual_host}" \
  --cargo-version "${actual_cargo}" \
  --rust-sysroot "${actual_sysroot}" \
  --rust-target-libdir "${actual_target_libdir}" \
  || fail "observed builder values differ from the pinned release contract"

[[ -d "${actual_sysroot}" && ! -L "${actual_sysroot}" ]] \
  || fail "Rust sysroot must be a real directory"
[[ -d "${actual_target_libdir}" && ! -L "${actual_target_libdir}" ]] \
  || fail "Rust target libdir must be a real directory"

printf 'Linux release builder contract ok: Ubuntu 22.04, glibc 2.35, Rust 1.97.1 (%s)\n' \
  "${target}"
