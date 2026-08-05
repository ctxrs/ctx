#!/usr/bin/env bash
set -euo pipefail
umask 077

usage() {
  cat >&2 <<'USAGE'
Usage: run-linux-bazel-release-controller.sh --platform <linux-x64|linux-arm64> --source-commit SHA --output-dir PATH --private-symbols-dir PATH --controller-receipt PATH

Launches the complete native Linux Bazel release transaction inside the
digest-pinned Ubuntu 22.04 outer controller. The physical machine supplies
only a native Docker daemon and mounted storage; it is recorded as the
launcher, not substituted for release construction authority. Output, private
symbols, receipt, work, cache, and advisory paths must be absolute host paths.
CTX_LINUX_RELEASE_WORK_ROOT and CTX_LINUX_RELEASE_CACHE_ROOT must name existing
writable non-symlink directories. This command does not publish anything.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

target_id=""
source_commit=""
output_dir=""
private_symbols_dir=""
controller_receipt=""
while (( $# > 0 )); do
  case "$1" in
    --platform)
      shift
      target_id="${1:-}"
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
    --controller-receipt)
      shift
      controller_receipt="${1:-}"
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

case "${target_id}" in
  linux-x64)
    platform=linux-x64
    expected_arch=x86_64
    docker_platform=linux/amd64
    docker_archive_arch=x86_64
    docker_archive_sha256=4f798b3ee1e0140eab5bf30b0edc4e84f4cdb53255a429dc3bbae9524845d640
    docker_binary_sha256=242c7a8de606afba2acada7c7af00d77f92c3601678b2f3a60911b49a892c722
    buildx_arch=amd64
    buildx_sha256=8c38f60308a895fa570f1410e453c5de11aafd65a99fa99965d96d24b6225a78
    zstd_binary_sha256=d304445daa7e6429293dc02035063b7993fb6a489ee90d8851bff497952836dc
    ;;
  linux-arm64)
    platform=linux-aarch64
    expected_arch=aarch64
    docker_platform=linux/arm64
    docker_archive_arch=aarch64
    docker_archive_sha256=e6b53725a73763ab3f988c73f8772eaed429754c1a579db5ff11f21990fd1817
    docker_binary_sha256=3e2d3307e386e59268ab1b17c195f5f224b7c616f06c553d22c0d10f90e5e618
    buildx_arch=arm64
    buildx_sha256=f7d867e9f1a3c00b32dd580f56594e229df05e3fb1b083b7099c91c2e7d2ce1e
    zstd_binary_sha256=50eed4c67aef71f5a33e82df66788f5415840c66827b6ef2fdf799a046ad59de
    ;;
  *)
    echo "error: --platform must be linux-x64 or linux-arm64" >&2
    exit 64
    ;;
esac
[[ "${source_commit}" =~ ^[0-9a-f]{40}$ \
  && ! "${source_commit}" =~ ^0{40}$ ]] \
  || die "--source-commit must be a nonzero lowercase 40-hex commit"

for variable in output_dir private_symbols_dir controller_receipt; do
  [[ -n "${!variable}" && "${!variable}" == /* ]] \
    || die "--${variable//_/-} must be an absolute path"
done
for command in docker git python3 sha256sum stat; do
  command -v "${command}" >/dev/null 2>&1 \
    || die "required controller launcher command is unavailable: ${command}"
done
[[ "$(uname -s)" == Linux ]] \
  || die "the pinned Linux controller requires a Linux Docker launch host"
docker_ambient_selectors=(
  DOCKER_HOST DOCKER_CONTEXT DOCKER_CONFIG DOCKER_CERT_PATH DOCKER_TLS \
  DOCKER_TLS_VERIFY DOCKER_DEFAULT_PLATFORM DOCKER_API_VERSION \
  BUILDX_BUILDER BUILDKIT_HOST
)
for selector in "${docker_ambient_selectors[@]}"; do
  [[ -z "${!selector+x}" ]] \
    || die "ambient Docker selector is forbidden: ${selector}"
done
docker_socket="${CTX_LINUX_RELEASE_DOCKER_SOCKET:-/var/run/docker.sock}"
[[ "${docker_socket}" == /* ]] || die "Docker socket path must be absolute"
[[ -S "${docker_socket}" && ! -L "${docker_socket}" ]] \
  || die "Docker socket is unavailable: ${docker_socket}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "${repo_root}"
[[ "$(git rev-parse --verify HEAD^{commit})" == "${source_commit}" ]] \
  || die "source commit does not match the controller checkout"
[[ -z "$(git status --porcelain=v1 --untracked-files=all)" ]] \
  || die "Linux controller requires a clean checkout"
source_tree="$(git rev-parse --verify HEAD^{tree})"

normalize_absent_destination() {
  local path="$1"
  local label="$2"
  local parent
  local leaf
  parent="$(dirname "${path}")"
  leaf="$(basename "${path}")"
  [[ -d "${parent}" && ! -L "${parent}" && -w "${parent}" ]] \
    || die "${label} parent must be a writable non-symlink directory"
  parent="$(cd "${parent}" && pwd -P)"
  [[ "${leaf}" != . && "${leaf}" != .. && "${leaf}" != */* ]] \
    || die "${label} has an invalid final component"
  path="${parent}/${leaf}"
  [[ ! -e "${path}" && ! -L "${path}" ]] \
    || die "${label} must not already exist: ${path}"
  printf '%s\n' "${path}"
}

output_dir="$(normalize_absent_destination "${output_dir}" "output directory")"
private_symbols_dir="$(
  normalize_absent_destination "${private_symbols_dir}" "private symbols directory"
)"
controller_receipt="$(
  normalize_absent_destination "${controller_receipt}" "controller receipt"
)"

for variable in \
  CTX_OSV_SCANNER CTX_OSV_DATABASE_DIR CTX_OSV_DATABASE_METADATA \
  CTX_LINUX_RELEASE_WORK_ROOT CTX_LINUX_RELEASE_CACHE_ROOT; do
  [[ -n "${!variable:-}" && "${!variable}" == /* ]] \
    || die "${variable} must be an absolute path"
done
osv_scanner="${CTX_OSV_SCANNER}"
osv_database="${CTX_OSV_DATABASE_DIR}"
osv_metadata="${CTX_OSV_DATABASE_METADATA}"
work_root="${CTX_LINUX_RELEASE_WORK_ROOT}"
cache_root="${CTX_LINUX_RELEASE_CACHE_ROOT}"
[[ -f "${osv_scanner}" && ! -L "${osv_scanner}" && -x "${osv_scanner}" ]] \
  || die "CTX_OSV_SCANNER must be an executable non-symlink file"
[[ -d "${osv_database}" && ! -L "${osv_database}" ]] \
  || die "CTX_OSV_DATABASE_DIR must be a non-symlink directory"
[[ -f "${osv_metadata}" && ! -L "${osv_metadata}" ]] \
  || die "CTX_OSV_DATABASE_METADATA must be a regular non-symlink file"
for variable in work_root cache_root; do
  [[ -d "${!variable}" && ! -L "${!variable}" && -w "${!variable}" ]] \
    || die "${variable//_/-} must be a writable non-symlink directory"
  printf -v "${variable}" '%s' "$(cd "${!variable}" && pwd -P)"
  [[ "${!variable}" != / ]] || die "${variable//_/-} must not be the filesystem root"
done
osv_scanner="$(cd "$(dirname "${osv_scanner}")" && pwd -P)/$(basename "${osv_scanner}")"
osv_database="$(cd "${osv_database}" && pwd -P)"
osv_metadata="$(cd "$(dirname "${osv_metadata}")" && pwd -P)/$(basename "${osv_metadata}")"
controller_task_prefix="${work_root}/ctx-linux-release-controller."
controller_task_root="$(mktemp -d "${controller_task_prefix}XXXXXX")"
read -r controller_task_device controller_task_inode \
  < <(stat -c '%d %i' -- "${controller_task_root}")
cleanup_controller_task() {
  if [[ "${controller_task_root:-}" == "${controller_task_prefix}"* \
    && -d "${controller_task_root}" && ! -L "${controller_task_root}" ]]; then
    python3 -I scripts/release/publish-linux-bazel-release.py \
      cleanup-task-root \
      --work-root "${work_root}" \
      --task-root "${controller_task_root}" \
      --expected-device "${controller_task_device}" \
      --expected-inode "${controller_task_inode}"
  fi
}
trap cleanup_controller_task EXIT
launcher_docker_config="${controller_task_root}/launcher-docker-config"
controller_docker_config="${controller_task_root}/controller-docker-config"
launcher_home="${controller_task_root}/launcher-home"
controller_home="${controller_task_root}/home"
controller_tmp="${controller_task_root}/tmp"
sidecar_cache="${cache_root}/onnxruntime-sidecar"
install -d -m 0700 \
  "${launcher_docker_config}" "${controller_docker_config}" \
  "${launcher_home}" "${controller_home}" "${controller_tmp}" \
  "${sidecar_cache}"
docker_host="unix://${docker_socket}"
docker_cli=(env)
for selector in "${docker_ambient_selectors[@]}"; do
  docker_cli+=(-u "${selector}")
done
docker_cli+=(
  "HOME=${launcher_home}"
  docker
  --host "${docker_host}"
  --config "${launcher_docker_config}"
)

socket_identity() {
  stat -c $'%d\t%i\t%f' -- "${docker_socket}"
}
docker_daemon_identity() {
  local raw arch version daemon_id
  raw="$(
    "${docker_cli[@]}" info --format \
      '{{printf "%s\t%s\t%s" .Architecture .ServerVersion .ID}}'
  )"
  IFS=$'\t' read -r arch version daemon_id <<<"${raw}"
  case "${arch}" in
    amd64|x86_64) arch=x86_64 ;;
    arm64|aarch64) arch=aarch64 ;;
    *) die "Docker daemon architecture is unsupported: ${arch:-missing}" ;;
  esac
  [[ -n "${version}" && -n "${daemon_id}" ]] \
    || die "Docker daemon identity is incomplete"
  printf '%s\t%s\t%s\n' "${arch}" "${version}" "${daemon_id}"
}
docker_socket_before="$(socket_identity)"
docker_daemon_before="$(docker_daemon_identity)"

IFS=$'\t' read -r \
  launcher_system launcher_arch launcher_native_arch launcher_translated \
  launcher_native_probe launcher_hardware launcher_emulation launcher_hypervisor \
  launcher_complete \
  < <(scripts/public-cli-host-runtime-evidence.sh)
launcher_evidence="${launcher_system}"$'\t'"${launcher_arch}"$'\t'"${launcher_native_arch}"$'\t'"${launcher_translated}"$'\t'"${launcher_native_probe}"$'\t'"${launcher_hardware}"$'\t'"${launcher_emulation}"$'\t'"${launcher_hypervisor}"$'\t'"${launcher_complete}"
launcher_os="$(scripts/public-cli-host-runtime-evidence.sh --os-baseline-only)"
IFS=$'\t' read -r launcher_os_id launcher_os_version launcher_os_product \
  <<<"${launcher_os}"
launcher_authority="$(
  scripts/public-cli-runtime-authority.sh \
    "${platform}" "${launcher_system}" "${launcher_arch}" passed \
    "${launcher_native_arch}" "${launcher_translated}" \
    "${launcher_hardware}" "${launcher_emulation}" "${launcher_hypervisor}" \
    "${launcher_complete}" "" \
    "${launcher_os_id}" "${launcher_os_version}" "${launcher_os_product}"
)"
case "${launcher_authority}" in
  authoritative|non_authoritative) ;;
  *) die "launcher authority classifier returned an invalid result" ;;
esac

IFS=$'\t' read -r daemon_arch daemon_version daemon_id \
  <<<"${docker_daemon_before}"
[[ "${daemon_arch}" == "${expected_arch}" ]] \
  || die "${platform} requires a native ${expected_arch} Docker daemon"

controller_base="docker.io/library/ubuntu:22.04@sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982"
controller_base_digest="${controller_base##*@}"
ubuntu_snapshot=20260701T000000Z
docker_version=27.5.1
buildx_version=0.20.1
controller_recipe=scripts/release/linux-bazel-release-controller.Dockerfile
controller_tag="ctx-public-cli-bazel:${platform}-controller-ubuntu-22.04"

"${docker_cli[@]}" pull --platform "${docker_platform}" \
  "${controller_base}" >/dev/null
actual_base_digest="$(
  "${docker_cli[@]}" image inspect "${controller_base}" \
    --format '{{range .RepoDigests}}{{println .}}{{end}}' \
    | sed -n 's/^.*@\(sha256:[0-9a-f]\{64\}\)$/\1/p' \
    | sort -u
)"
[[ "${actual_base_digest}" == "${controller_base_digest}" ]] \
  || die "resolved controller base does not match its pinned digest"

"${docker_cli[@]}" build \
  --platform "${docker_platform}" \
  --provenance=false \
  --build-arg "UBUNTU_IMAGE=${controller_base}" \
  --build-arg "UBUNTU_SNAPSHOT=${ubuntu_snapshot}" \
  --build-arg "CONTROLLER_ARCH=${expected_arch}" \
  --build-arg "DOCKER_ARCH=${docker_archive_arch}" \
  --build-arg "DOCKER_SHA256=${docker_archive_sha256}" \
  --build-arg "DOCKER_VERSION=${docker_version}" \
  --build-arg "BUILDX_ARCH=${buildx_arch}" \
  --build-arg "BUILDX_SHA256=${buildx_sha256}" \
  --build-arg "BUILDX_VERSION=${buildx_version}" \
  -t "${controller_tag}" \
  -f "${controller_recipe}" \
  scripts/release
controller_image_id="$(
  "${docker_cli[@]}" image inspect "${controller_tag}" --format '{{.Id}}'
)"
[[ "${controller_image_id}" =~ ^sha256:[0-9a-f]{64}$ ]] \
  || die "controller image did not resolve to an immutable image ID"
[[ "$("${docker_cli[@]}" image inspect "${controller_image_id}" --format '{{.Id}}')" \
  == "${controller_image_id}" ]] \
  || die "controller image ID does not resolve exactly"
for label_contract in \
  "org.ctx.release.arch=${expected_arch}" \
  "org.ctx.release.base-image=${controller_base}" \
  "org.ctx.release.buildx-version=${buildx_version}" \
  "org.ctx.release.docker-version=${docker_version}" \
  "org.ctx.release.role=ctx-public-bazel-controller" \
  "org.ctx.release.ubuntu-snapshot=${ubuntu_snapshot}"; do
  label="${label_contract%%=*}"
  expected="${label_contract#*=}"
  actual="$(
    "${docker_cli[@]}" image inspect "${controller_image_id}" \
      --format "{{index .Config.Labels \"${label}\"}}"
  )"
  [[ "${actual}" == "${expected}" ]] \
    || die "controller image label mismatch for ${label}"
done

socket_gid="$(stat -c '%g' "${docker_socket}")"
controller_common=(
  run
  --rm
  --platform "${docker_platform}"
  --user "$(id -u):$(id -g)"
  --group-add "${socket_gid}"
  --cap-drop ALL
  --security-opt no-new-privileges
  --read-only
  --tmpfs /tmp:rw,nosuid,nodev,exec
  -e "HOME=${controller_home}"
  -e USER=ctx-controller
  -e LOGNAME=ctx-controller
  -e "TMPDIR=${controller_tmp}"
  -e DOCKER_HOST=unix:///run/ctx-release/docker.sock
  -e "DOCKER_CONFIG=${controller_docker_config}"
  -e CTX_LINUX_RELEASE_DOCKER_SOCKET=/run/ctx-release/docker.sock
  -e "CTX_LINUX_RELEASE_DOCKER_CONFIG=${controller_docker_config}"
  -v "${docker_socket}:/run/ctx-release/docker.sock"
  -v "${repo_root}:${repo_root}:ro"
  -v "${work_root}:${work_root}:rw"
  -w "${repo_root}"
)

controller_probe() {
  "${docker_cli[@]}" "${controller_common[@]}" \
    --network none \
    "${controller_image_id}" \
    bash -ceu '
      platform="$1"
      docker_host="${DOCKER_HOST}"
      docker_config="${DOCKER_CONFIG}"
      docker_cli=(
        env -u DOCKER_HOST -u DOCKER_CONTEXT -u DOCKER_CONFIG
        -u DOCKER_CERT_PATH -u DOCKER_TLS -u DOCKER_TLS_VERIFY
        -u DOCKER_DEFAULT_PLATFORM -u DOCKER_API_VERSION
        -u BUILDX_BUILDER -u BUILDKIT_HOST
        "HOME=${HOME}" docker --host "${docker_host}" --config "${docker_config}"
      )
      IFS=$'"'"'\t'"'"' read -r \
        system arch native_arch translated native_probe hardware emulation \
        hypervisor complete \
        < <(scripts/public-cli-host-runtime-evidence.sh)
      evidence="${system}"$'"'"'\t'"'"'"${arch}"$'"'"'\t'"'"'"${native_arch}"$'"'"'\t'"'"'"${translated}"$'"'"'\t'"'"'"${native_probe}"$'"'"'\t'"'"'"${hardware}"$'"'"'\t'"'"'"${emulation}"$'"'"'\t'"'"'"${hypervisor}"$'"'"'\t'"'"'"${complete}"
      os="$(scripts/public-cli-host-runtime-evidence.sh --os-baseline-only)"
      IFS=$'"'"'\t'"'"' read -r os_id os_version os_product <<<"${os}"
      authority="$(
        scripts/public-cli-runtime-authority.sh \
          "${platform}" "${system}" "${arch}" passed "${native_arch}" \
          "${translated}" "${hardware}" "${emulation}" "${hypervisor}" \
          "${complete}" "" "${os_id}" "${os_version}" "${os_product}"
      )"
      client_version="$("${docker_cli[@]}" --version)"
      client_sha="$(sha256sum /usr/local/bin/docker | awk '"'"'{print $1}'"'"')"
      buildx_version="$("${docker_cli[@]}" buildx version)"
      buildx_sha="$(
        sha256sum /usr/local/lib/docker/cli-plugins/docker-buildx \
          | awk '"'"'{print $1}'"'"'
      )"
      zstd_version="$(zstd --version)"
      zstd_sha="$(sha256sum /usr/bin/zstd | awk '"'"'{print $1}'"'"')"
      daemon="$(
        "${docker_cli[@]}" info --format \
          '"'"'{{printf "%s\t%s\t%s" .Architecture .ServerVersion .ID}}'"'"'
      )"
      socket="$(
        stat -c $'"'"'%d\t%i\t%f'"'"' "${CTX_LINUX_RELEASE_DOCKER_SOCKET}"
      )"
      printf '"'"'evidence\t%s\nos\t%s\nauthority\t%s\n'"'"' \
        "${evidence}" "${os}" "${authority}"
      printf '"'"'docker_client_version\t%s\ndocker_client_sha\t%s\n'"'"' \
        "${client_version}" "${client_sha}"
      printf '"'"'buildx_version\t%s\nbuildx_sha\t%s\n'"'"' \
        "${buildx_version}" "${buildx_sha}"
      printf '"'"'zstd_version\t%s\nzstd_sha\t%s\ndaemon\t%s\nsocket\t%s\n'"'"' \
        "${zstd_version}" "${zstd_sha}" "${daemon}" "${socket}"
    ' controller-preflight "${platform}"
}
controller_preflight="$(controller_probe)" \
  || die "pinned Ubuntu 22 controller preflight failed"

controller_evidence=""
controller_os=""
controller_authority=""
controller_docker_version=""
controller_docker_sha=""
controller_buildx_version=""
controller_buildx_sha=""
controller_zstd_version=""
controller_zstd_sha=""
controller_daemon=""
controller_socket=""
while IFS=$'\t' read -r key value; do
  case "${key}" in
    evidence) controller_evidence="${value}" ;;
    os) controller_os="${value}" ;;
    authority) controller_authority="${value}" ;;
    docker_client_version) controller_docker_version="${value}" ;;
    docker_client_sha) controller_docker_sha="${value}" ;;
    buildx_version) controller_buildx_version="${value}" ;;
    buildx_sha) controller_buildx_sha="${value}" ;;
    zstd_version) controller_zstd_version="${value}" ;;
    zstd_sha) controller_zstd_sha="${value}" ;;
    daemon) controller_daemon="${value}" ;;
    socket) controller_socket="${value}" ;;
    *) die "controller preflight returned an unknown field" ;;
  esac
done <<<"${controller_preflight}"
[[ "${controller_authority}" == authoritative ]] \
  || die "pinned Ubuntu 22 outer controller is not authoritative"
[[ "${controller_os}" == $'ubuntu\t22.04\tunknown' ]] \
  || die "pinned outer controller did not report Ubuntu 22.04"
[[ "${controller_docker_sha}" == "${docker_binary_sha256}" ]] \
  || die "controller Docker client digest does not match the pinned tool"
[[ "${controller_buildx_sha}" == "${buildx_sha256}" ]] \
  || die "controller Buildx digest does not match the pinned tool"
[[ "${controller_docker_version}" == "Docker version ${docker_version},"* ]] \
  || die "controller Docker client version does not match the pinned tool"
[[ "${controller_buildx_version}" == *"v${buildx_version}"* ]] \
  || die "controller Buildx version does not match the pinned tool"
[[ "${controller_zstd_version}" == *"zstd command line interface"* \
  && "${controller_zstd_version}" == *"v1.4.8,"* ]] \
  || die "controller zstd version does not match the pinned snapshot tool"
[[ "${controller_zstd_sha}" == "${zstd_binary_sha256}" ]] \
  || die "controller zstd digest does not match the pinned snapshot tool"
IFS=$'\t' read -r controller_daemon_arch controller_daemon_version \
  controller_daemon_id <<<"${controller_daemon}"
case "${controller_daemon_arch}" in
  amd64|x86_64) controller_daemon_arch=x86_64 ;;
  arm64|aarch64) controller_daemon_arch=aarch64 ;;
esac
[[ "${controller_daemon_arch}" == "${expected_arch}" \
  && -n "${controller_daemon_version}" && -n "${controller_daemon_id}" ]] \
  || die "controller Docker daemon evidence is incomplete or non-native"
controller_daemon_identity="${controller_daemon_arch}"$'\t'"${controller_daemon_version}"$'\t'"${controller_daemon_id}"
[[ "${controller_daemon_identity}" == "${docker_daemon_before}" ]] \
  || die "controller Docker context does not resolve to the validated launcher daemon"
IFS=$'\t' read -r controller_socket_device_before \
  controller_socket_inode_before controller_socket_mode_before \
  <<<"${controller_socket}"
[[ -n "${controller_socket_device_before}" \
  && -n "${controller_socket_inode_before}" \
  && -n "${controller_socket_mode_before}" ]] \
  || die "controller Docker socket evidence is incomplete"

git_common_dir="$(git rev-parse --path-format=absolute --git-common-dir)"
controller_run=("${controller_common[@]}")
controller_run+=(
  -e "CTX_OSV_SCANNER=${osv_scanner}"
  -e "CTX_OSV_DATABASE_DIR=${osv_database}"
  -e "CTX_OSV_DATABASE_METADATA=${osv_metadata}"
  -e "CTX_LINUX_RELEASE_WORK_ROOT=${work_root}"
  -e "CTX_LINUX_RELEASE_CACHE_ROOT=${cache_root}"
  -e "CTX_ONNXRUNTIME_CACHE_DIR=${sidecar_cache}"
  -v "${osv_scanner}:${osv_scanner}:ro"
  -v "${osv_database}:${osv_database}:ro"
  -v "${osv_metadata}:${osv_metadata}:ro"
)
declare -A controller_rw_mounts=(["${work_root}"]=1)
for mount_path in \
  "${cache_root}" \
  "$(dirname "${output_dir}")" \
  "$(dirname "${private_symbols_dir}")"; do
  if [[ -z "${controller_rw_mounts[${mount_path}]+x}" ]]; then
    controller_run+=(-v "${mount_path}:${mount_path}:rw")
    controller_rw_mounts["${mount_path}"]=1
  fi
done
case "${git_common_dir}/" in
  "${repo_root}/"*) ;;
  *) controller_run+=(-v "${git_common_dir}:${git_common_dir}:ro") ;;
esac

"${docker_cli[@]}" "${controller_run[@]}" \
  "${controller_image_id}" \
  scripts/release/build-linux-bazel-release.sh \
    --platform "${target_id}" \
    --source-commit "${source_commit}" \
    --output-dir "${output_dir}" \
    --private-symbols-dir "${private_symbols_dir}"

[[ "$(git rev-parse --verify HEAD^{commit})" == "${source_commit}" \
  && "$(git rev-parse --verify HEAD^{tree})" == "${source_tree}" \
  && -z "$(git status --porcelain=v1 --untracked-files=all)" ]] \
  || die "source checkout changed during controller construction"
docker_socket_after="$(socket_identity)"
docker_daemon_after="$(docker_daemon_identity)"
[[ "${docker_socket_after}" == "${docker_socket_before}" ]] \
  || die "Docker Unix socket authority changed during construction"
[[ "${docker_daemon_after}" == "${docker_daemon_before}" ]] \
  || die "Docker daemon authority changed during construction"
controller_postflight="$(controller_probe)" \
  || die "pinned Ubuntu 22 controller postflight failed"
[[ "${controller_postflight}" == "${controller_preflight}" ]] \
  || die "controller Docker or toolchain authority changed during construction"
controller_socket_after="$(
  sed -n $'s/^socket\t//p' <<<"${controller_postflight}"
)"
IFS=$'\t' read -r controller_socket_device_after \
  controller_socket_inode_after controller_socket_mode_after \
  <<<"${controller_socket_after}"
IFS=$'\t' read -r socket_device_before socket_inode_before socket_mode_before \
  <<<"${docker_socket_before}"
IFS=$'\t' read -r socket_device_after socket_inode_after socket_mode_after \
  <<<"${docker_socket_after}"
python3 -I scripts/release/write-linux-bazel-controller-receipt.py \
  --buildx-sha256 "${controller_buildx_sha}" \
  --buildx-version "${controller_buildx_version}" \
  --candidate-dir "${output_dir}" \
  --controller-authority "${controller_authority}" \
  --controller-base-image "${controller_base}" \
  --controller-evidence "${controller_evidence}" \
  --controller-image-id "${controller_image_id}" \
  --controller-os "${controller_os}" \
  --controller-recipe "${controller_recipe}" \
  --controller-socket-device-after "${controller_socket_device_after}" \
  --controller-socket-device-before "${controller_socket_device_before}" \
  --controller-socket-inode-after "${controller_socket_inode_after}" \
  --controller-socket-inode-before "${controller_socket_inode_before}" \
  --controller-socket-mode-after "${controller_socket_mode_after}" \
  --controller-socket-mode-before "${controller_socket_mode_before}" \
  --daemon-after "${docker_daemon_after}" \
  --daemon-before "${docker_daemon_before}" \
  --docker-client-sha256 "${controller_docker_sha}" \
  --docker-client-version "${controller_docker_version}" \
  --launcher-authority "${launcher_authority}" \
  --launcher-evidence "${launcher_evidence}" \
  --launcher-os "${launcher_os}" \
  --output "${controller_receipt}" \
  --platform "${platform}" \
  --socket-device-after "${socket_device_after}" \
  --socket-device-before "${socket_device_before}" \
  --socket-inode-after "${socket_inode_after}" \
  --socket-inode-before "${socket_inode_before}" \
  --socket-mode-after "${socket_mode_after}" \
  --socket-mode-before "${socket_mode_before}" \
  --source-commit "${source_commit}" \
  --source-tree "${source_tree}" \
  --zstd-sha256 "${controller_zstd_sha}" \
  --zstd-version "${controller_zstd_version}"

[[ "$(socket_identity)" == "${docker_socket_before}" ]] \
  || die "Docker Unix socket authority changed while writing the receipt"
[[ "$(docker_daemon_identity)" == "${docker_daemon_before}" ]] \
  || die "Docker daemon authority changed while writing the receipt"

printf 'authoritative %s controller receipt: %s\n' \
  "${platform}" "${controller_receipt}"
trap - EXIT
cleanup_controller_task
