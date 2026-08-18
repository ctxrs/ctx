#!/usr/bin/env bash
set -euo pipefail

readonly RUST_VERSION="1.97.1"
readonly RUST_COMMIT="8bab26f4f68e0e26f0bb7960be334d5b520ea452"
readonly ZIG_VERSION="0.15.2"
readonly ZIG_SHA256="02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239"
readonly CARGO_ZIGBUILD_VERSION="0.23.0"
readonly RCODESIGN_VERSION="0.29.0"
readonly RCODESIGN_SHA256="dbe85cedd8ee4217b64e9a0e4c2aef92ab8bcaaa41f20bde99781ff02e600002"
readonly JSIGN_VERSION="7.5"
readonly JSIGN_SHA256="602a51c3545a6dc4fb99bd2ea7152b26d1345916d0c93ddfbd5936cb735af91c"
readonly JSIGN_URL="https://github.com/ebourg/jsign/releases/download/7.5/jsign-7.5.jar"
readonly TOMLI_VERSION="2.0.1"
readonly TOMLI_SHA256="939de3e7a6161af0c887ef91b7d41a53e7c5a1ca976325f429cb46ea9bc30ecc"
readonly FACTORY_INPUTS="contracts/release-factory-inputs-v1.json"

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/release/build-public-candidate-on-linux.sh [OPTIONS]

Builds the selected public ctx CLI binaries on Linux x86_64 and writes one Core
candidate directory. By default all five targets are selected. Native platform
jobs validate these exact bytes; they do not rebuild them.

Options:
  --source-commit SHA      Required clean source commit (default: HEAD)
  --output-dir DIR         Candidate directory (default: target/public-cli-artifacts)
  --toolchain-dir DIR      Verified tool cache (default: target/release-toolchain)
  --targets ID[,ID...]     Build selected target IDs; repeat for more (default: all)
  --macos-sdk PATH         Private regular macOS SDK archive
  --jobs N                 Cargo jobs per target (default: 2)
  --build-parallelism N    Concurrent target builds (default: 2)
  --diagnostic-unsigned    Build and inspect, but do not sign or emit releasable manifests

Official mode requires CTX_OSV_SCANNER, CTX_OSV_DATABASE_DIR,
CTX_OSV_DATABASE_METADATA, and the Ubuntu 24.04 x86_64 host declared in
contracts/release-factory-inputs-v1.json. Selecting a macOS target also
requires --macos-sdk; official macOS selection signs and notarizes those
selected binaries. Official Windows selection applies Azure Artifact Signing
on this Linux host before checksums and manifests are sealed. A filtered
candidate is non-promotable. Semantic models and runtime transports are
constructed only by the separate Semantic release graph; this factory never
constructs or seals them.
The host may be a direct installation, VM, container, or Buildkite image.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

sha256_file() {
  sha256sum "$1" | awk '{print $1}'
}

verify_sha256() {
  local path="$1" expected="$2" actual
  actual="$(sha256_file "${path}")"
  [[ "${actual}" == "${expected}" ]] || \
    die "SHA-256 mismatch for ${path}: expected ${expected}, got ${actual}"
}

download_verified() {
  local url="$1" expected="$2" output="$3" temporary
  temporary="${output}.tmp.$$"
  mkdir -p "$(dirname "${output}")"
  if [[ -f "${output}" ]]; then
    verify_sha256 "${output}" "${expected}"
    return
  fi
  rm -f "${temporary}"
  curl --fail --location --retry 4 --retry-all-errors --silent --show-error \
    "${url}" --output "${temporary}"
  verify_sha256 "${temporary}" "${expected}"
  mv "${temporary}" "${output}"
}

source_commit=""
output_dir="target/public-cli-artifacts"
toolchain_dir="target/release-toolchain"
macos_sdk_input="${CTX_MACOS_SDK_ROOT:-}"
cargo_jobs="${CTX_RELEASE_CARGO_JOBS:-2}"
build_parallelism="${CTX_RELEASE_BUILD_PARALLELISM:-2}"
target_specs=()
official=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --source-commit) shift; source_commit="${1:-}" ;;
    --output-dir) shift; output_dir="${1:-}" ;;
    --toolchain-dir) shift; toolchain_dir="${1:-}" ;;
    --targets)
      shift
      [[ $# -gt 0 ]] || die "--targets requires at least one target ID"
      target_specs+=("$1")
      ;;
    --macos-sdk) shift; macos_sdk_input="${1:-}" ;;
    --jobs) shift; cargo_jobs="${1:-}" ;;
    --build-parallelism) shift; build_parallelism="${1:-}" ;;
    --diagnostic-unsigned) official=0 ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 2 ;;
  esac
  shift
done
[[ "${cargo_jobs}" =~ ^[1-9][0-9]*$ ]] || die "--jobs must be positive"
[[ "${build_parallelism}" =~ ^[1-9][0-9]*$ ]] || die "--build-parallelism must be positive"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${repo_root}"
[[ "$(uname -s)" == "Linux" ]] || die "release factory requires Linux"
case "$(uname -m)" in x86_64|amd64) ;; *) die "release factory requires x86_64" ;; esac
require_command python3
all_target_ids_text="$(python3 scripts/public-cli-release-targets.py targets --field id)" || \
  die "public CLI release-target matrix is unavailable"
mapfile -t all_target_ids <<<"${all_target_ids_text}"
[[ "${#all_target_ids[@]}" -gt 0 && -n "${all_target_ids[0]}" ]] || \
  die "public CLI release-target matrix has no targets"
declare -A known_target_ids=()
for target_id in "${all_target_ids[@]}"; do
  known_target_ids["${target_id}"]=1
done
target_ids=()
declare -A selected_target_ids=()
if [[ "${#target_specs[@]}" == "0" ]]; then
  target_ids=("${all_target_ids[@]}")
else
  for target_spec in "${target_specs[@]}"; do
    if [[ "${target_spec}" == "all" ]]; then
      [[ "${#target_specs[@]}" == "1" ]] || \
        die "--targets all cannot be combined with target IDs"
      target_ids=("${all_target_ids[@]}")
      break
    fi
    [[ "${target_spec}" != ,* && "${target_spec}" != *, && "${target_spec}" != *,,* ]] || \
      die "--targets contains an empty target ID"
    IFS=',' read -r -a requested_target_ids <<<"${target_spec}"
    for target_id in "${requested_target_ids[@]}"; do
      [[ -n "${target_id}" ]] || die "--targets contains an empty target ID"
      [[ -n "${known_target_ids[${target_id}]+present}" ]] || \
        die "unsupported release target: ${target_id}"
      [[ -z "${selected_target_ids[${target_id}]+present}" ]] || \
        die "duplicate release target: ${target_id}"
      selected_target_ids["${target_id}"]=1
      target_ids+=("${target_id}")
    done
  done
  [[ "${#target_ids[@]}" -gt 0 ]] || die "--targets selected no targets"
fi
needs_macos=0
needs_windows=0
for target_id in "${target_ids[@]}"; do
  [[ "${target_id}" == macos-* ]] && needs_macos=1
  [[ "${target_id}" == windows-x64 ]] && needs_windows=1
done
selection_complete=1
if [[ "${#target_ids[@]}" != "${#all_target_ids[@]}" ]]; then
  selection_complete=0
else
  for target_id in "${all_target_ids[@]}"; do
    if [[ "${#target_specs[@]}" == "0" || "${target_specs[0]}" == "all" ]]; then
      break
    fi
    [[ -n "${selected_target_ids[${target_id}]+present}" ]] || {
      selection_complete=0
      break
    }
  done
fi
source_commit="${source_commit:-$(git rev-parse --verify HEAD^{commit})}"
[[ "${source_commit}" =~ ^[0-9a-f]{40}$ && ! "${source_commit}" =~ ^0{40}$ ]] || \
  die "source commit must be nonzero lowercase 40-hex"
[[ "$(git rev-parse --verify HEAD^{commit})" == "${source_commit}" ]] || \
  die "factory checkout does not match --source-commit"
[[ -z "$(git status --porcelain=v1 --untracked-files=all)" ]] || \
  die "official release factory requires a clean checkout"

factory_input_json="$(cat "${FACTORY_INPUTS}")" || die "factory input contract is unavailable"
factory_host_arch="$(python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["linux_host"]["arch"])' <<<"${factory_input_json}")"
factory_host_authority="$(python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["linux_host"]["authority"])' <<<"${factory_input_json}")"
factory_host_os_id="$(python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["linux_host"]["os_id"])' <<<"${factory_input_json}")"
factory_host_os_version="$(python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["linux_host"]["os_version"])' <<<"${factory_input_json}")"
macos_sdk_authority=""
macos_sdk_expected_size=""
macos_sdk_expected_sha256=""
if [[ "${needs_macos}" == "1" ]]; then
  macos_sdk_authority="$(python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["macos_sdk"]["authority"])' <<<"${factory_input_json}")"
  macos_sdk_expected_size="$(python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["macos_sdk"]["archive_size_bytes"])' <<<"${factory_input_json}")"
  macos_sdk_expected_sha256="$(python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["macos_sdk"]["archive_sha256"])' <<<"${factory_input_json}")"
  [[ "${macos_sdk_authority}" =~ ^[a-z0-9.-]+$ ]] || die "factory SDK authority is malformed"
  [[ "${macos_sdk_expected_size}" =~ ^[1-9][0-9]*$ ]] || die "factory SDK size is malformed"
  [[ "${macos_sdk_expected_sha256}" =~ ^[0-9a-f]{64}$ ]] || die "factory SDK digest is malformed"
fi
[[ "${factory_host_arch}" == "x86_64" ]] || die "factory host architecture is malformed"
[[ "${factory_host_authority}" =~ ^[a-z0-9._-]+$ ]] || die "factory host authority is malformed"
[[ "${factory_host_os_id}" == "ubuntu" ]] || die "factory host OS identity is malformed"
[[ "${factory_host_os_version}" =~ ^[0-9]+\.[0-9]+$ ]] || die "factory host OS version is malformed"
factory_host_os="${factory_host_os_id}-${factory_host_os_version}-${factory_host_arch}"

for command_name in cargo cat curl file git install llvm-objdump llvm-readobj llvm-strip \
  python3 rustc rustup sha256sum tar xz; do
  require_command "${command_name}"
done
if [[ "${official}" == "1" && "${needs_macos}" == "1" ]]; then
  require_command openssl
fi
if [[ "${official}" == "1" && "${needs_windows}" == "1" ]]; then
  require_command java
fi
rust_release="$(rustc --version --verbose | sed -n 's/^release: //p')"
rust_commit="$(rustc --version --verbose | sed -n 's/^commit-hash: //p')"
[[ "${rust_release}" == "${RUST_VERSION}" && "${rust_commit}" == "${RUST_COMMIT}" ]] || \
  die "rustc must be pinned ${RUST_VERSION} (${RUST_COMMIT})"
rust_version="$(rustc --version)"
builder_authority="${CTX_RELEASE_BUILDER_AUTHORITY:-${factory_host_authority}}"
inspector_authority="${CTX_RELEASE_INSPECTOR_AUTHORITY:-ctx-release-static-llvm-v1}"
inspector_tool="$(llvm-readobj --version | head -n 1)"
[[ -n "${builder_authority}" && -n "${inspector_authority}" && -n "${inspector_tool}" ]] || \
  die "factory evidence authorities must be non-empty"

mkdir -p "${toolchain_dir}"
tomli_dir="${toolchain_dir}/python-tomli-${TOMLI_VERSION}-site-packages"
tomli_wheel="${toolchain_dir}/tomli-${TOMLI_VERSION}-py3-none-any.whl"
download_verified \
  "https://files.pythonhosted.org/packages/97/75/10a9ebee3fd790d20926a90a2547f0bf78f371b2f13aa822c759680ca7b9/tomli-${TOMLI_VERSION}-py3-none-any.whl" \
  "${TOMLI_SHA256}" "${tomli_wheel}"
if [[ ! -f "${tomli_dir}/tomli/__init__.py" ]]; then
  tomli_temporary="$(mktemp -d "${toolchain_dir}/python-tomli.XXXXXX")"
  python3 -m zipfile -e "${tomli_wheel}" "${tomli_temporary}"
  [[ -f "${tomli_temporary}/tomli/__init__.py" ]] || \
    die "tomli wheel has an unexpected layout"
  mv "${tomli_temporary}" "${tomli_dir}"
fi
python_with_tomli=(env "PYTHONPATH=${repo_root}/${tomli_dir}" python3 -S)
[[ "$("${python_with_tomli[@]}" -c 'import tomli; print(tomli.__version__)')" == "${TOMLI_VERSION}" ]] || \
  die "tomli version mismatch"

zig_dir="${toolchain_dir}/zig-x86_64-linux-${ZIG_VERSION}"
if [[ ! -x "${zig_dir}/zig" ]]; then
  zig_archive="${toolchain_dir}/zig-x86_64-linux-${ZIG_VERSION}.tar.xz"
  download_verified \
    "https://ziglang.org/download/${ZIG_VERSION}/zig-x86_64-linux-${ZIG_VERSION}.tar.xz" \
    "${ZIG_SHA256}" "${zig_archive}"
  tar -C "${toolchain_dir}" -xf "${zig_archive}"
fi
export PATH="${repo_root}/${zig_dir}:${PATH}"
[[ "$(zig version)" == "${ZIG_VERSION}" ]] || die "Zig version mismatch"

if ! cargo install --list | grep -Fqx "cargo-zigbuild v${CARGO_ZIGBUILD_VERSION}:"; then
  if [[ "${CTX_RELEASE_ALLOW_TOOL_INSTALL:-0}" != "1" ]]; then
    die "cargo-zigbuild ${CARGO_ZIGBUILD_VERSION} is required (set CTX_RELEASE_ALLOW_TOOL_INSTALL=1 to install it)"
  fi
  cargo install cargo-zigbuild --version "${CARGO_ZIGBUILD_VERSION}" --locked
fi
cargo_zigbuild_resolution="$(python3 scripts/release/resolve-cargo-zigbuild.py \
  --expected-version "${CARGO_ZIGBUILD_VERSION}")"
cargo_zigbuild_bin="$(python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["path"])' \
  <<<"${cargo_zigbuild_resolution}")"
cargo_zigbuild_observed_version="$(python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["observed_version"])' \
  <<<"${cargo_zigbuild_resolution}")"

if [[ "${official}" == "1" && "${needs_macos}" == "1" ]]; then
  rcodesign_dir="${toolchain_dir}/apple-codesign-${RCODESIGN_VERSION}-x86_64-unknown-linux-musl"
  if [[ ! -x "${rcodesign_dir}/rcodesign" ]]; then
    rcodesign_archive="${toolchain_dir}/apple-codesign-${RCODESIGN_VERSION}-x86_64-unknown-linux-musl.tar.gz"
    download_verified \
      "https://github.com/indygreg/apple-platform-rs/releases/download/apple-codesign/${RCODESIGN_VERSION}/apple-codesign-${RCODESIGN_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
      "${RCODESIGN_SHA256}" "${rcodesign_archive}"
    mkdir -p "${rcodesign_dir}"
    tar -C "${rcodesign_dir}" -xzf "${rcodesign_archive}"
    nested_rcodesign="${rcodesign_dir}/apple-codesign-${RCODESIGN_VERSION}-x86_64-unknown-linux-musl/rcodesign"
    [[ -x "${nested_rcodesign}" ]] || die "rcodesign archive has an unexpected layout"
    mv "${nested_rcodesign}" "${rcodesign_dir}/rcodesign"
  fi
  export PATH="${repo_root}/${rcodesign_dir}:${PATH}"
  rcodesign --version | grep -F "${RCODESIGN_VERSION}" >/dev/null || \
    die "rcodesign version mismatch"
fi

jsign_jar=""
if [[ "${official}" == "1" && "${needs_windows}" == "1" ]]; then
  jsign_jar="${toolchain_dir}/jsign-${JSIGN_VERSION}.jar"
  download_verified "${JSIGN_URL}" "${JSIGN_SHA256}" "${jsign_jar}"
  export CTX_WINDOWS_JSIGN_JAR="${jsign_jar}"
  scripts/run-windows-release-signing.sh --preflight
fi

macos_sdk_root=""
sdk_cleanup=""
macos_sdk_sha256=""
if [[ "${needs_macos}" == "1" ]]; then
  if [[ -n "${macos_sdk_input}" ]]; then
    [[ "${macos_sdk_input}" == /* ]] || macos_sdk_input="${repo_root}/${macos_sdk_input}"
    if [[ -f "${macos_sdk_input}" && ! -L "${macos_sdk_input}" ]]; then
      sdk_cleanup="$(mktemp -d "${TMPDIR:-/tmp}/ctx-macos-sdk.XXXXXX")"
      trap 'rm -rf "${sdk_cleanup:-}"' EXIT
      sdk_archive="${sdk_cleanup}/MacOSX.sdk.archive"
      install -m 0600 -- "${macos_sdk_input}" "${sdk_archive}"
      [[ "$(stat -c '%s' "${sdk_archive}")" == "${macos_sdk_expected_size}" ]] || \
        die "macOS SDK archive size mismatch: expected ${macos_sdk_expected_size}"
      verify_sha256 "${sdk_archive}" "${macos_sdk_expected_sha256}"
      sdk_extract="${sdk_cleanup}/extracted"
      mkdir -p "${sdk_extract}"
      tar --warning=no-unknown-keyword -C "${sdk_extract}" -xf "${sdk_archive}"
      mapfile -t sdk_matches < <(find "${sdk_extract}" -type d -name 'MacOSX*.sdk' -print)
      [[ "${#sdk_matches[@]}" == "1" ]] || die "macOS SDK archive must contain exactly one MacOSX*.sdk"
      macos_sdk_root="${sdk_matches[0]}"
    else
      die "macOS SDK input must be a regular archive"
    fi
  fi
  [[ -n "${macos_sdk_root}" && -d "${macos_sdk_root}/System/Library/Frameworks" ]] || \
    die "--macos-sdk must provide a complete MacOSX SDK"
  macos_sdk_sha256="${macos_sdk_expected_sha256}"
fi

selected_triples=()
for target_id in "${target_ids[@]}"; do
  target_values="$(python3 scripts/public-cli-release-targets.py shell "${target_id}")"
  eval "${target_values}"
  selected_triples+=("${CTX_PUBLIC_TARGET_TRIPLE}")
done
for triple in "${selected_triples[@]}"; do
  rustup target list --installed | grep -Fx "${triple}" >/dev/null || \
    die "Rust target ${triple} is not installed"
done

if [[ "${official}" == "1" ]]; then
  . /etc/os-release
  [[ "${ID:-}" == "${factory_host_os_id}" \
    && "${VERSION_ID:-}" == "${factory_host_os_version}" ]] || \
    die "official release factory requires Ubuntu ${factory_host_os_version}"
  [[ "${builder_authority}" == "${factory_host_authority}" ]] || \
    die "official release factory builder authority does not match its input contract"
  for required_name in \
    CTX_OSV_SCANNER CTX_OSV_DATABASE_DIR CTX_OSV_DATABASE_METADATA; do
    [[ -n "${!required_name:-}" ]] || die "official release requires ${required_name}"
  done
fi

mkdir -p "$(dirname "${output_dir}")"
[[ ! -e "${output_dir}" ]] || die "output directory already exists: ${output_dir}"
stage_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-linux-release-factory.XXXXXX")"
cleanup() {
  rm -rf "${stage_dir:-}" "${sdk_cleanup:-}" >/dev/null 2>&1 || true
}
trap cleanup EXIT
artifact_stage="${stage_dir}/artifacts"
mkdir -p "${artifact_stage}"
cargo_lock_sha256="$(sha256_file Cargo.lock)"
version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; d=json.load(sys.stdin); print(next(p["version"] for p in d["packages"] if p["name"]=="ctx"))')"

build_target() {
  local target_id="$1" platform triple build_triple binary raw target_dir
  local encoded_flags
  local CTX_PUBLIC_TARGET_PLATFORM CTX_PUBLIC_TARGET_TRIPLE CTX_PUBLIC_TARGET_BINARY
  local -a build_env
  eval "$(python3 "${repo_root}/scripts/public-cli-release-targets.py" shell "${target_id}")"
  platform="${CTX_PUBLIC_TARGET_PLATFORM}"
  triple="${CTX_PUBLIC_TARGET_TRIPLE}"
  build_triple="${triple}"
  [[ "${target_id}" != linux-* ]] || build_triple="${triple}.2.35"
  binary="${CTX_PUBLIC_TARGET_BINARY}"
  raw="ctx"
  [[ "${target_id}" != "windows-x64" ]] || raw="ctx.exe"
  target_dir="${repo_root}/target/linux-release-factory/${target_id}"
  encoded_flags=""
  if [[ "${target_id}" == macos-* ]]; then
    # Leave deterministic load-command space for the Developer ID signature.
    # Without this, the x86_64 Mach-O can place __text immediately after the
    # existing commands, leaving rcodesign nowhere to add LC_CODE_SIGNATURE.
    encoded_flags="-Clink-arg=-Wl,-headerpad,0x1000"
  fi
  build_env=(
    "CARGO_TARGET_DIR=${target_dir}"
    "CTX_RELEASE_BUILD_SOURCE_COMMIT=${source_commit}"
    "CTX_RELEASE_BUILD_CARGO_LOCK_SHA256=${cargo_lock_sha256}"
    "CTX_RELEASE_BUILD_TARGET=${triple}"
  )
  if [[ "${target_id}" == macos-* ]]; then
    build_env+=("SDKROOT=${macos_sdk_root}" "MACOSX_DEPLOYMENT_TARGET=13.0")
    build_env+=("CARGO_ENCODED_RUSTFLAGS=${encoded_flags}")
  fi
  env "${build_env[@]}" \
    "${cargo_zigbuild_bin}" zigbuild --manifest-path "${repo_root}/Cargo.toml" \
      -p ctx --bin ctx --release --locked --target "${build_triple}" -j "${cargo_jobs}"
  if [[ "${target_id}" == macos-* ]]; then
    # Cargo's release profile strips debug data, but the Linux cross-link can
    # retain Mach-O private nlist entries. Remove those before signing and
    # before the static artifact contract; keep the UUID for native symbols.
    llvm-strip -S -x "${target_dir}/${triple}/release/${raw}"
  fi
  install -m 0755 "${target_dir}/${triple}/release/${raw}" "${artifact_stage}/${binary}"
  if [[ "${target_id}" == "linux-x64" ]]; then
    "${artifact_stage}/${binary}" --version >"${artifact_stage}/${binary}.version"
    grep -Fx "ctx ${version}" "${artifact_stage}/${binary}.version" >/dev/null
  else
    printf 'not run on this host: %s\n' "${platform}" >"${artifact_stage}/${binary}.version"
  fi
}
pids=()
for target_id in "${target_ids[@]}"; do
  build_target "${target_id}" &
  pids+=("$!")
  if [[ "${#pids[@]}" -ge "${build_parallelism}" ]]; then
    wait "${pids[0]}"
    pids=("${pids[@]:1}")
  fi
done
for pid in "${pids[@]}"; do
  wait "${pid}"
done

if [[ "${official}" == "1" && "${needs_macos}" == "1" ]]; then
  export CTX_MACOS_RELEASE_SOURCE_COMMIT="${source_commit}"
  for target_id in "${target_ids[@]}"; do
    case "${target_id}" in
      macos-*)
        eval "$(python3 scripts/public-cli-release-targets.py shell "${target_id}")"
        scripts/run-macos-release-signing.sh "${target_id}" cli \
          "${artifact_stage}/${CTX_PUBLIC_TARGET_BINARY}" "${artifact_stage}"
        ;;
    esac
  done
fi

if [[ "${official}" == "1" && "${needs_windows}" == "1" ]]; then
  eval "$(python3 scripts/public-cli-release-targets.py shell windows-x64)"
  scripts/run-windows-release-signing.sh cli \
    "${artifact_stage}/${CTX_PUBLIC_TARGET_BINARY}" "${artifact_stage}"
fi

for target_id in "${target_ids[@]}"; do
  eval "$(python3 scripts/public-cli-release-targets.py shell "${target_id}")"
  platform="${CTX_PUBLIC_TARGET_PLATFORM}"
  binary="${CTX_PUBLIC_TARGET_BINARY}"
  artifact="${artifact_stage}/${binary}"
  sha256_file "${artifact}" >"${artifact}.sha256"
  llvm_args=()
  if [[ "${platform}" == "macos-x64" ]]; then
    # Linux is the construction authority for every target. The macOS x64
    # native validator/published-release checker may use its approved Darwin
    # snapshot, but this factory uses the fixed Linux package-root pair.
    llvm_args=(/usr/bin/llvm-readobj /usr/bin/llvm-objdump)
  fi
  CTX_PUBLIC_CLI_EXPECTED_VERSION="${version}" \
    scripts/check-public-cli-artifact.sh "${platform}" "${artifact_stage}" "${llvm_args[@]}"
  if [[ "${official}" == "1" ]]; then
    inventory="${stage_dir}/${target_id}.cargo-inventory.json"
    materials="${stage_dir}/${target_id}.cargo-materials.json"
    material_root="${stage_dir}/${target_id}.cargo-materials"
    python3 scripts/release/cargo-release-inventory.py \
      --repo "${repo_root}" --target "${CTX_PUBLIC_TARGET_TRIPLE}" \
      --target-output "${inventory}" --materials-output "${materials}" \
      --material-root "${material_root}"
    build_info_args=(
      --artifact "${artifact}" --cargo-lock Cargo.lock \
      --matrix contracts/release-targets-v1.json \
      --output "${artifact}.build-info.json" --platform "${platform}" \
      --recipe scripts/release/build-public-candidate-on-linux.sh \
      --rust-version "${rust_version}" --source-commit "${source_commit}" \
      --source-repo "${repo_root}" --static-status passed \
      --local-runtime-status not_run --local-runtime-authority not_run \
      --zig-version "${ZIG_VERSION}" \
      --cargo-zigbuild-version "${cargo_zigbuild_observed_version}" \
      --builder-authority "${builder_authority}" \
      --builder-os "${factory_host_os}" \
      --inspector-authority "${inspector_authority}" \
      --inspector-tool "${inspector_tool}"
    )
    if [[ "${target_id}" == macos-* ]]; then
      build_info_args+=(
        --macos-sdk-sha256 "${macos_sdk_sha256}"
        --macos-sdk-authority "${macos_sdk_authority}"
      )
    fi
    python3 scripts/release/linux-factory-build-info.py "${build_info_args[@]}"
    "${python_with_tomli[@]}" scripts/dependency-advisory-gate.py \
      --repo-root "${repo_root}" \
      --policy security/release-advisory-policy-v1.json \
      --exceptions security/release-advisory-exceptions-v1.json \
      --database-root "${CTX_OSV_DATABASE_DIR}" \
      --database-metadata "${CTX_OSV_DATABASE_METADATA}" \
      --scanner "${CTX_OSV_SCANNER}" --cargo-inventory "${inventory}" \
      --target-id "${target_id}" --output "${artifact}.dependency-advisory.json"
    "${python_with_tomli[@]}" scripts/release-sbom.py generate \
      --product core --version "${version}" --target-id "${target_id}" \
      --platform "${platform}" --artifact "${artifact}" \
      --build-info "${artifact}.build-info.json" --cargo-lock Cargo.lock \
      --module-file MODULE.bazel --module-lock MODULE.bazel.lock \
      --target-inventory "${inventory}" --license-materials "${materials}" \
      --runfiles-root "${material_root}" \
      --target-matrix contracts/release-targets-v1.json \
      --candidate-schema contracts/release-candidate-manifest-v1.schema.json \
      --workspace-manifest Cargo.toml \
      --index-manifest crates/ctx-history-index/Cargo.toml \
      --index-format-manifest crates/ctx-history-index-format/Cargo.toml \
      --index-query-manifest crates/ctx-history-index-query/Cargo.toml \
      --output "${artifact}.cdx.json" \
      --notices-output "${artifact}.third-party-notices.txt" \
      --size-report-output "${artifact}.size.json" \
      --candidate-manifest "${artifact}.candidate.json"
    sha256_file "${artifact}.cdx.json" >"${artifact}.cdx.json.sha256"
    sha256_file "${artifact}.third-party-notices.txt" >"${artifact}.third-party-notices.txt.sha256"
  fi
done

candidate_releasable=0
if [[ "${official}" == "1" && "${selection_complete}" == "1" ]]; then
  candidate_releasable=1
fi

python3 - "${artifact_stage}" "${source_commit}" "${official}" \
  "${selection_complete}" "${version}" "${target_ids[@]}" <<'PY'
import hashlib, json, os, sys
from pathlib import Path
root = Path(sys.argv[1])
commit = sys.argv[2]
official = sys.argv[3] == "1"
selection_complete = sys.argv[4] == "1"
version = sys.argv[5]
selected_targets = sys.argv[6:]
files=[]
for path in sorted(root.iterdir(), key=lambda item: item.name):
    if path.is_file() and not path.name.startswith("."):
        files.append({"file":path.name,"sha256":hashlib.sha256(path.read_bytes()).hexdigest(),"size_bytes":path.stat().st_size})
doc={
    "schema_version": 1,
    "kind": "ctx-linux-release-factory",
    "source_commit": commit,
    "version": version,
    "selected_targets": selected_targets,
    "releasable": official and selection_complete,
    "runtime_sidecars_included": False,
    "files": files,
}
(root/"ctx-release-factory.json").write_text(json.dumps(doc,sort_keys=True,separators=(",",":"))+"\n")
PY
if [[ "${candidate_releasable}" == "1" ]]; then
  python3 -I scripts/release/seal-linux-factory-candidate.py \
    --candidate-dir "${artifact_stage}" \
    --source-commit "${source_commit}" >/dev/null
fi
mv "${artifact_stage}" "${output_dir}"
trap - EXIT
rm -rf "${stage_dir}" "${sdk_cleanup:-}"
factory_status="non-promotable"
if [[ "${official}" != "1" ]]; then
  factory_status="diagnostic-unsigned"
elif [[ "${candidate_releasable}" == "1" ]]; then
  factory_status="releasable"
fi
printf 'Linux release factory complete: %s (%s)\n' \
  "${output_dir}" "${factory_status}"
