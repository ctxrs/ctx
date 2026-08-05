#!/usr/bin/env bash
set -euo pipefail
umask 077

usage() {
  cat >&2 <<'USAGE'
Usage: build-linux-bazel-release.sh --platform <linux-x64|linux-arm64> --source-commit SHA --output-dir PATH [--private-symbols-dir PATH]

Builds and packages one native Linux Core candidate through the matching
//:ctx_release_<target> --config=release route. The package version and output
name come from the tracked source and release-target contract. This command
requires absolute CTX_OSV_SCANNER, CTX_OSV_DATABASE_DIR, and
CTX_OSV_DATABASE_METADATA inputs for its offline advisory gate. It does not
sign, upload, publish, deploy, or update a release channel.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

platform=""
source_commit=""
output_dir=""
private_symbols_dir=""
while (( $# > 0 )); do
  case "$1" in
    --platform)
      shift
      platform="${1:-}"
      ;;
    --source-commit)
      shift
      source_commit="${1:-}"
      ;;
    --output-dir)
      shift
      output_dir="${1:-}"
      ;;
    --private-symbols-dir)
      shift
      private_symbols_dir="${1:-}"
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'error: unknown argument: %s\n' "$1" >&2
      usage
      exit 64
      ;;
  esac
  shift
done

case "${platform}" in
  linux-x64)
    expected_host_arch=x86_64
    docker_platform=linux/amd64
    bazel_binary_arch=x86_64
    bazel_binary_sha256=c97f02133adce63f0c28678ac1f21d65fa8255c80429b588aeeba8a1fac6202b
    route_target=//:ctx_release_linux_x64
    route_binary=ctx_release_linux_x64
    ;;
  linux-arm64)
    expected_host_arch=aarch64
    docker_platform=linux/arm64
    bazel_binary_arch=arm64
    bazel_binary_sha256=d7aedc8565ed47b6231badb80b09f034e389c5f2b1c2ac2c55406f7c661d8b88
    route_target=//:ctx_release_linux_arm64
    route_binary=ctx_release_linux_arm64
    ;;
  *)
    echo "error: --platform must be linux-x64 or linux-arm64" >&2
    exit 64
    ;;
esac
[[ "${source_commit}" =~ ^[0-9a-f]{40}$ \
  && ! "${source_commit}" =~ ^0{40}$ ]] || {
  echo "error: --source-commit must be a nonzero lowercase 40-hex commit" >&2
  exit 64
}
[[ -n "${output_dir}" ]] || {
  echo "error: --output-dir is required" >&2
  exit 64
}
[[ "$(uname -s)" == "Linux" ]] \
  || die "native Linux Bazel release construction requires Linux"
[[ "$(uname -m)" == "${expected_host_arch}" ]] \
  || die "${platform} construction requires a native ${expected_host_arch} host"
for command in docker flock git python3 sha256sum; do
  command -v "${command}" >/dev/null 2>&1 \
    || die "required builder command is unavailable: ${command}"
done

osv_scanner_input="${CTX_OSV_SCANNER:-}"
osv_database_input="${CTX_OSV_DATABASE_DIR:-}"
osv_metadata_input="${CTX_OSV_DATABASE_METADATA:-}"
for variable in \
  CTX_OSV_SCANNER CTX_OSV_DATABASE_DIR CTX_OSV_DATABASE_METADATA; do
  [[ -n "${!variable:-}" ]] \
    || die "native Linux release construction requires ${variable}"
  [[ "${!variable}" == /* ]] \
    || die "${variable} must be an absolute path"
done
[[ -f "${osv_scanner_input}" && ! -L "${osv_scanner_input}" \
  && -x "${osv_scanner_input}" ]] \
  || die "CTX_OSV_SCANNER must be an executable non-symlink file"
[[ -d "${osv_database_input}" && ! -L "${osv_database_input}" ]] \
  || die "CTX_OSV_DATABASE_DIR must be a non-symlink directory"
[[ -f "${osv_metadata_input}" && ! -L "${osv_metadata_input}" ]] \
  || die "CTX_OSV_DATABASE_METADATA must be a regular non-symlink file"
osv_scanner_input="$(
  cd "$(dirname "${osv_scanner_input}")"
  printf '%s/%s\n' "$(pwd -P)" "$(basename "${osv_scanner_input}")"
)"
osv_database_input="$(cd "${osv_database_input}" && pwd -P)"
osv_metadata_input="$(
  cd "$(dirname "${osv_metadata_input}")"
  printf '%s/%s\n' "$(pwd -P)" "$(basename "${osv_metadata_input}")"
)"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "${repo_root}"
[[ "$(git rev-parse --verify HEAD^{commit})" == "${source_commit}" ]] \
  || die "source commit does not match the builder checkout"
[[ -z "$(git status --porcelain=v1 --untracked-files=all)" ]] \
  || die "native Linux Bazel release construction requires a clean checkout"

version="$(
  python3 -I scripts/release/public-cli-bazel-build-info.py cargo-version \
    --cargo-toml "${repo_root}/crates/ctx-cli/Cargo.toml"
)" || die "could not determine the ctx package version"

target_values="$(
  python3 scripts/public-cli-release-targets.py \
    --matrix contracts/release-targets-v1.json shell "${platform}"
)" || exit $?
eval "${target_values}"
[[ "${CTX_PUBLIC_TARGET_OS}:${CTX_PUBLIC_TARGET_ARCH}" \
  == "linux:${expected_host_arch}" ]] \
  || die "release-target matrix does not select the requested native Linux graph"
[[ "${CTX_PUBLIC_TARGET_LINUX_BUILDER_IMAGE}" == *@sha256:* \
  && -n "${CTX_PUBLIC_TARGET_LINUX_UBUNTU_SNAPSHOT}" \
  && -n "${CTX_PUBLIC_TARGET_GLIBC_MAX}" \
  && -n "${CTX_PUBLIC_TARGET_LINUX_RUST_TOOLCHAIN}" \
  && -n "${CTX_PUBLIC_TARGET_LINUX_RUST_COMMIT}" ]] \
  || die "release-target matrix lacks the pinned Linux build contract"

if [[ "${output_dir}" != /* ]]; then
  output_dir="${repo_root}/${output_dir}"
fi
output_dir="$(
  python3 - "${output_dir}" <<'PY'
import os
import sys

print(os.path.abspath(sys.argv[1]))
PY
)"
if [[ -z "${private_symbols_dir}" ]]; then
  private_symbols_dir="${output_dir}.private-debug-symbols"
fi
[[ "${private_symbols_dir}" == /* ]] \
  || die "--private-symbols-dir must be absolute"
[[ ! -e "${private_symbols_dir}" && ! -L "${private_symbols_dir}" ]] \
  || die "private symbol output must not already exist"
private_symbols_parent="$(dirname "${private_symbols_dir}")"
mkdir -p "${private_symbols_parent}"
symbols_stage_parent=""
case "${output_dir}/" in
  "${repo_root}/"*)
    git check-ignore -q -- "${output_dir}/.ctx-release-output" \
      || die "release output inside the checkout must be ignored by Git"
    ;;
esac
[[ ! -L "${output_dir}" ]] || die "release output directory is a symlink"
mkdir -p "${output_dir}"
[[ -d "${output_dir}" && -w "${output_dir}" ]] \
  || die "release output directory is not writable"

release_work_root="${CTX_LINUX_RELEASE_WORK_ROOT:-/tmp}"
[[ "${release_work_root}" == /* ]] \
  || die "CTX_LINUX_RELEASE_WORK_ROOT must be an absolute path"
[[ -d "${release_work_root}" && ! -L "${release_work_root}" \
  && -w "${release_work_root}" ]] \
  || die "Linux release work root must be a writable non-symlink directory"
release_work_root="$(cd "${release_work_root}" && pwd -P)"
[[ "${release_work_root}" != / ]] \
  || die "Linux release work root must not be the filesystem root"

bazel_version="$(tr -d '[:space:]' <.bazelversion)"
base_image="${CTX_PUBLIC_TARGET_LINUX_BUILDER_IMAGE}"
ubuntu_snapshot="${CTX_PUBLIC_TARGET_LINUX_UBUNTU_SNAPSHOT}"
glibc_max="${CTX_PUBLIC_TARGET_GLIBC_MAX}"
rust_toolchain="${CTX_PUBLIC_TARGET_LINUX_RUST_TOOLCHAIN}"
rust_commit="${CTX_PUBLIC_TARGET_LINUX_RUST_COMMIT}"
expected_base_digest="${base_image##*@}"

IFS=$'\t' read -r \
  host_system host_arch host_native_arch process_translated _native_arch_probe \
  hardware_identity emulation hypervisor evidence_complete \
  < <(scripts/public-cli-host-runtime-evidence.sh)
host_authority="$(
  scripts/public-cli-runtime-authority.sh \
    "${CTX_PUBLIC_TARGET_PLATFORM}" "${host_system}" "${host_arch}" passed \
    "${host_native_arch}" "${process_translated}" "${hardware_identity}" \
    "${emulation}" "${hypervisor}" "${evidence_complete}"
)"
[[ "${host_authority}" == "authoritative" ]] \
  || die "${platform} host evidence is not authoritative; emulation is diagnostic only"

daemon_arch="$(docker info --format '{{.Architecture}}')"
case "${daemon_arch}" in
  amd64|x86_64) daemon_arch=x86_64 ;;
  arm64|aarch64) daemon_arch=aarch64 ;;
esac
[[ "${daemon_arch}" == "${expected_host_arch}" ]] \
  || die "${platform} requires a native ${expected_host_arch} Docker daemon"

builder_image="ctx-public-cli-bazel:${platform}-builder-bazel-${bazel_version}"
runtime_image="ctx-public-cli-bazel:${platform}-runtime-ubuntu-22.04"
inspector_image="ctx-public-cli-bazel:${platform}-inspector-ubuntu-22.04"
builder_recipe="scripts/release/linux-bazel-release.Dockerfile"

docker pull --platform "${docker_platform}" "${base_image}" >/dev/null
actual_base_digest="$(
  docker image inspect "${base_image}" \
    --format '{{range .RepoDigests}}{{println .}}{{end}}' \
    | sed -n 's/^.*@\(sha256:[0-9a-f]\{64\}\)$/\1/p' \
    | sort -u
)"
[[ "${actual_base_digest}" == "${expected_base_digest}" ]] || {
  printf 'error: resolved Ubuntu base mismatch: expected %s, got %s\n' \
    "${expected_base_digest}" "${actual_base_digest:-missing}" >&2
  exit 1
}

docker build \
  --platform "${docker_platform}" \
  --target builder \
  --provenance=false \
  --build-arg "UBUNTU_IMAGE=${base_image}" \
  --build-arg "UBUNTU_SNAPSHOT=${ubuntu_snapshot}" \
  --build-arg "GLIBC_BASELINE=${glibc_max}" \
  --build-arg "BAZEL_VERSION=${bazel_version}" \
  --build-arg "BAZEL_ARCH=${bazel_binary_arch}" \
  --build-arg "BAZEL_SHA256=${bazel_binary_sha256}" \
  --build-arg "RELEASE_ARCH=${CTX_PUBLIC_TARGET_ARCH}" \
  --build-arg "RUST_TOOLCHAIN=${rust_toolchain}" \
  --build-arg "RUST_COMMIT=${rust_commit}" \
  -t "${builder_image}" \
  -f "${builder_recipe}" \
  scripts/release
docker build \
  --platform "${docker_platform}" \
  --target runtime \
  --provenance=false \
  --build-arg "UBUNTU_IMAGE=${base_image}" \
  --build-arg "UBUNTU_SNAPSHOT=${ubuntu_snapshot}" \
  --build-arg "RELEASE_ARCH=${CTX_PUBLIC_TARGET_ARCH}" \
  -t "${runtime_image}" \
  -f "${builder_recipe}" \
  scripts/release
docker build \
  --platform "${docker_platform}" \
  --target inspector \
  --provenance=false \
  --build-arg "UBUNTU_IMAGE=${base_image}" \
  --build-arg "UBUNTU_SNAPSHOT=${ubuntu_snapshot}" \
  --build-arg "RELEASE_ARCH=${CTX_PUBLIC_TARGET_ARCH}" \
  -t "${inspector_image}" \
  -f "${builder_recipe}" \
  scripts/release

builder_image_id="$(docker image inspect "${builder_image}" --format '{{.Id}}')"
runtime_image_id="$(docker image inspect "${runtime_image}" --format '{{.Id}}')"
inspector_image_id="$(docker image inspect "${inspector_image}" --format '{{.Id}}')"
for value in "${builder_image_id}" "${runtime_image_id}" "${inspector_image_id}"; do
  [[ "${value}" =~ ^sha256:[0-9a-f]{64}$ ]] \
    || die "release image did not resolve to an immutable image ID"
done

lock_file="/tmp/ctx-public-${platform}-bazel-release.lock"
exec 9>"${lock_file}"
flock -x 9
cache_root="${CTX_LINUX_RELEASE_CACHE_ROOT:-}"
if [[ -n "${cache_root}" ]]; then
  [[ "${cache_root}" == /* ]] || {
    echo "error: CTX_LINUX_RELEASE_CACHE_ROOT must be an absolute path" >&2
    exit 1
  }
  [[ -d "${cache_root}" && ! -L "${cache_root}" && -w "${cache_root}" ]] || {
    echo "error: Linux release cache root must be a writable non-symlink directory" >&2
    exit 1
  }
  cache_root="$(cd "${cache_root}" && pwd -P)"
  [[ "${cache_root}" != / ]] || {
    echo "error: Linux release cache root must not be the filesystem root" >&2
    exit 1
  }
fi
task_prefix="${release_work_root}/ctx-public-${platform}-bazel-release."
task_root="$(mktemp -d "${task_prefix}XXXXXX")"
cache_root="${cache_root:-${task_root}/cache}"
cleanup() {
  if [[ "${task_root:-}" == "${task_prefix}"* \
    && -d "${task_root}" && ! -L "${task_root}" ]]; then
    chmod -R u+w -- "${task_root}" 2>/dev/null || true
    rm -rf -- "${task_root}"
  fi
  if [[ -n "${symbols_stage_parent:-}" \
    && "${symbols_stage_parent}" == "${private_symbols_parent}/.ctx-symbol-stage."* \
    && -d "${symbols_stage_parent}" && ! -L "${symbols_stage_parent}" ]]; then
    rm -rf -- "${symbols_stage_parent}"
  fi
}
trap cleanup EXIT
symbols_stage_parent="$(mktemp -d "${private_symbols_parent}/.ctx-symbol-stage.XXXXXX")"
install -d -m 0700 \
  "${task_root}/release-input" \
  "${cache_root}"

docker_run_args=(
  --rm
  --platform "${docker_platform}"
  --user "$(id -u):$(id -g)"
  --cap-drop ALL
  --security-opt no-new-privileges
  --read-only
  --tmpfs /tmp:rw,nosuid,nodev,exec
  -e HOME=/tmp/home
  -e USER=ctx-builder
  -e LOGNAME=ctx-builder
  -e TMPDIR=/tmp
  -e CTX_BAZEL_BIN=/opt/ctx/bin/bazel
  -e CTX_BAZEL_CACHE_ROOT=/build/cache
  -e "CTX_RELEASE_ROUTE_TARGET=${route_target}"
  -e "CTX_RELEASE_ROUTE_BINARY=${route_binary}"
  -e "CTX_RELEASE_TARGET_ID=${CTX_PUBLIC_TARGET_ID}"
  -e "CTX_RELEASE_BINARY_NAME=${CTX_PUBLIC_TARGET_BINARY}"
  -e CTX_OSV_SCANNER=/release-advisory/osv-scanner
  -e CTX_OSV_DATABASE_DIR=/release-advisory/database
  -e CTX_OSV_DATABASE_METADATA=/release-advisory/database-metadata.json
  -v "${repo_root}:${repo_root}:ro"
  -v "${task_root}:/build:rw"
  -v "${osv_scanner_input}:/release-advisory/osv-scanner:ro"
  -v "${osv_database_input}:/release-advisory/database:ro"
  -v "${osv_metadata_input}:/release-advisory/database-metadata.json:ro"
  -v "${symbols_stage_parent}:/release-symbol-output:rw"
  -w "${repo_root}"
)
if [[ "${cache_root}" != "${task_root}/cache" ]]; then
  docker_run_args+=(-v "${cache_root}:/build/cache:rw")
fi
git_common_dir="$(git rev-parse --path-format=absolute --git-common-dir)"
case "${git_common_dir}/" in
  "${repo_root}/"*) ;;
  *) docker_run_args+=(-v "${git_common_dir}:${git_common_dir}:ro") ;;
esac

docker run "${docker_run_args[@]}" \
  "${builder_image_id}" \
  bash -ceu '
    install -d -m 0700 "$HOME"
    scripts/bazelw fetch \
      "${CTX_RELEASE_ROUTE_TARGET}" \
      --config=release \
      --lockfile_mode=error \
      --symlink_prefix=/build/bazel-links/
  '

docker run "${docker_run_args[@]}" \
  --network none \
  "${builder_image_id}" \
  bash -ceu '
    install -d -m 0700 "$HOME"
    wrapper=scripts/bazelw
    "$wrapper" build \
      "${CTX_RELEASE_ROUTE_TARGET}" \
      --config=release \
      --lockfile_mode=error \
      --symlink_prefix=/build/bazel-links/
    bazel_bin="$(
      "$wrapper" info bazel-bin \
        --config=release \
        --lockfile_mode=error \
        --symlink_prefix=/build/bazel-links/
    )"
    route_runfiles="$bazel_bin/${CTX_RELEASE_ROUTE_BINARY}.runfiles"
    artifact_runfile="$(
      find "$route_runfiles" \
        -path "*/ctx_release_routes/${CTX_RELEASE_TARGET_ID}/artifact" \
        -print
    )"
    rustc_runfile="$(
      find "$route_runfiles" \
        -path "*/ctx_release_routes/${CTX_RELEASE_TARGET_ID}/rustc" \
        -print
    )"
    test -n "$artifact_runfile"
    test "$(printf "%s\n" "$artifact_runfile" | wc -l)" -eq 1
    test -n "$rustc_runfile"
    test "$(printf "%s\n" "$rustc_runfile" | wc -l)" -eq 1
    test -f "$artifact_runfile" -a -x "$artifact_runfile"
    test -f "$rustc_runfile" -a -x "$rustc_runfile"
    install -m 0755 \
      "$artifact_runfile" "/build/release-input/${CTX_RELEASE_BINARY_NAME}"
    python3 scripts/release/detached-debug-symbols.py prepare \
      --artifact "/build/release-input/${CTX_RELEASE_BINARY_NAME}" \
      --output-dir /build/release-input/private-debug-symbols \
      --platform "${CTX_RELEASE_TARGET_ID}" \
      --product ctx
    "$rustc_runfile" --version > /build/release-input/rustc.version
  '

artifact="${task_root}/release-input/${CTX_PUBLIC_TARGET_BINARY}"
rust_version="$(tr -d '\r\n' <"${task_root}/release-input/rustc.version")"
artifact_sha256="$(sha256sum "${artifact}" | awk '{print $1}')"
printf '%s\n' "${artifact_sha256}" >"${artifact}.sha256"
"${artifact}" --version >"${artifact}.version"
grep -Fx "ctx ${version}" "${artifact}.version" >/dev/null \
  || die "Bazel artifact does not report the requested version"

# The build-info producer deliberately runs its pinned container gates as the
# unprivileged nobody user. Expose only this public candidate, its required
# sidecars, and their containing release-input directory.
chmod 0555 "${artifact}"
chmod 0444 "${artifact}.sha256" "${artifact}.version"
chmod 0755 "${task_root}/release-input"

build_info="${artifact}.build-info.json"
python3 -I scripts/release/public-cli-bazel-build-info.py create \
  --artifact "${artifact}" \
  --bazel-version-file .bazelversion \
  --builder-image-id "${builder_image_id}" \
  --builder-recipe "${builder_recipe}" \
  --cargo-lock Cargo.lock \
  --cargo-toml crates/ctx-cli/Cargo.toml \
  --docker "$(command -v docker)" \
  --inspector-image-id "${inspector_image_id}" \
  --matrix contracts/release-targets-v1.json \
  --module-file MODULE.bazel \
  --module-lock MODULE.bazel.lock \
  --output "${build_info}" \
  --platform "${CTX_PUBLIC_TARGET_PLATFORM}" \
  --runtime-image-id "${runtime_image_id}" \
  --rust-version "${rust_version}" \
  --source-commit "${source_commit}" \
  --source-repo "${repo_root}" \
  --version "${version}"

docker run "${docker_run_args[@]}" \
  --network none \
  -v "${output_dir}:/release-output:rw" \
  "${builder_image_id}" \
  bash -ceu '
    install -d -m 0700 "$HOME"
    route="/build/bazel-links/bin/${CTX_RELEASE_ROUTE_BINARY}"
    test -x "$route"
    test -d "$route.runfiles"
    BUILD_WORKSPACE_DIRECTORY="$PWD" \
    RUNFILES_DIR="$route.runfiles" \
    TEST_WORKSPACE=_main \
    "$route" \
      --build-info "/build/release-input/${CTX_RELEASE_BINARY_NAME}.build-info.json" \
      --output-dir /release-output \
      --private-symbols-dir /release-symbol-output/bundle
  '

[[ -d "${symbols_stage_parent}/bundle" ]] \
  || die "packaged release output is missing private debug symbols"
[[ ! -e "${private_symbols_dir}" && ! -L "${private_symbols_dir}" ]] \
  || die "private symbol output appeared during construction"
mv "${symbols_stage_parent}/bundle" "${private_symbols_dir}"

[[ "$(git rev-parse --verify HEAD^{commit})" == "${source_commit}" ]] \
  || die "source commit changed during native Linux Bazel construction"
[[ -z "$(git status --porcelain=v1 --untracked-files=all)" ]] \
  || die "source checkout changed during native Linux Bazel construction"
for leaf in \
  "${CTX_PUBLIC_TARGET_BINARY}" \
  "${CTX_PUBLIC_TARGET_BINARY}.sha256" \
  "${CTX_PUBLIC_TARGET_BINARY}.version" \
  "${CTX_PUBLIC_TARGET_BINARY}.build-info.json" \
  "${CTX_PUBLIC_TARGET_BINARY}.cdx.json" \
  "${CTX_PUBLIC_TARGET_BINARY}.cdx.json.sha256" \
  "${CTX_PUBLIC_TARGET_BINARY}.third-party-notices.txt" \
  "${CTX_PUBLIC_TARGET_BINARY}.third-party-notices.txt.sha256" \
  "${CTX_PUBLIC_TARGET_BINARY}.size.json" \
  "${CTX_PUBLIC_TARGET_BINARY}.candidate.json"; do
  [[ -s "${output_dir}/${leaf}" ]] \
    || die "packaged release output is missing: ${output_dir}/${leaf}"
done

trap - EXIT
cleanup
printf 'native %s Core Bazel candidate: %s\n' \
  "${platform}" "${output_dir}/${CTX_PUBLIC_TARGET_BINARY}"
printf 'source commit: %s\n' "${source_commit}"
printf 'version: %s\n' "${version}"

exit 0
