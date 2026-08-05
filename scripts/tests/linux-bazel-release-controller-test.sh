#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  source_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  source_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fi

grep -Fq '{{printf "%s\t%s\t%s" .Architecture .ServerVersion .ID}}' \
  "${source_root}/scripts/release/run-linux-bazel-release-controller.sh"

tmp_dir="$(mktemp -d)"
socket_pid=""
cleanup() {
  if [[ -n "${socket_pid}" ]]; then
    kill "${socket_pid}" >/dev/null 2>&1 || true
    wait "${socket_pid}" >/dev/null 2>&1 || true
  fi
  rm -rf "${tmp_dir}"
}
trap cleanup EXIT

fixture="${tmp_dir}/source"
mkdir -p \
  "${fixture}/scripts/release" \
  "${tmp_dir}/advisory/database" \
  "${tmp_dir}/cache" \
  "${tmp_dir}/fake-bin" \
  "${tmp_dir}/outputs" \
  "${tmp_dir}/work"
install -m 0755 \
  "${source_root}/scripts/public-cli-host-runtime-evidence.sh" \
  "${source_root}/scripts/public-cli-runtime-authority.sh" \
  "${fixture}/scripts"
install -m 0755 \
  "${source_root}/scripts/release/run-linux-bazel-release-controller.sh" \
  "${source_root}/scripts/release/publish-linux-bazel-release.py" \
  "${source_root}/scripts/release/write-linux-bazel-controller-receipt.py" \
  "${fixture}/scripts/release"
install -m 0644 \
  "${source_root}/scripts/release/completed_candidate_io.py" \
  "${source_root}/scripts/release/linux-bazel-release-controller.Dockerfile" \
  "${fixture}/scripts/release"
cat >"${fixture}/scripts/release/build-linux-bazel-release.sh" <<'EOF'
#!/usr/bin/env bash
echo "the fake Docker transport must intercept this command" >&2
exit 99
EOF
chmod 0755 "${fixture}/scripts/release/build-linux-bazel-release.sh"
printf '__pycache__/\n' >"${fixture}/.gitignore"
git -C "${fixture}" init -q
git -C "${fixture}" add .
git -C "${fixture}" \
  -c user.name='ctx controller test' \
  -c user.email='ctx-controller-test@example.invalid' \
  commit -qm 'create controller fixture'
source_commit="$(git -C "${fixture}" rev-parse HEAD)"

printf 'scanner\n' >"${tmp_dir}/advisory/osv-scanner"
chmod 0755 "${tmp_dir}/advisory/osv-scanner"
printf '{}\n' >"${tmp_dir}/advisory/database-metadata.json"

socket_path="${tmp_dir}/docker.sock"
python3 - "${socket_path}" <<'PY' &
import socket
import sys
import time

listener = socket.socket(socket.AF_UNIX)
listener.bind(sys.argv[1])
listener.listen()
time.sleep(300)
PY
socket_pid=$!
for _attempt in $(seq 1 100); do
  [[ -S "${socket_path}" ]] && break
  sleep 0.01
done
[[ -S "${socket_path}" ]]

cat >"${tmp_dir}/fake-bin/docker" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf '%q ' "$@" >>"${FAKE_DOCKER_LOG}"
printf '\n' >>"${FAKE_DOCKER_LOG}"
while [[ "${1:-}" == --host || "${1:-}" == --config ]]; do
  shift 2
done
controller_id="sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
base="docker.io/library/ubuntu:22.04@sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982"
daemon_arch="${FAKE_DAEMON_ARCH:-x86_64}"
daemon_id="${FAKE_DAEMON_ID:-fixture-daemon}"

if [[ "${1:-}" == info ]]; then
  count=0
  [[ ! -f "${FAKE_DOCKER_INFO_COUNT}" ]] \
    || count="$(<"${FAKE_DOCKER_INFO_COUNT}")"
  count=$((count + 1))
  printf '%s\n' "${count}" >"${FAKE_DOCKER_INFO_COUNT}"
  if [[ "${FAKE_MUTATE_DAEMON_AFTER:-0}" == 1 && "${count}" -ge 2 ]]; then
    daemon_id=mutated-daemon
  fi
  printf '%s\t29.1.3\t%s\n' "${daemon_arch}" "${daemon_id}"
  exit 0
fi
if [[ "${1:-}" == pull || "${1:-}" == build ]]; then
  exit 0
fi
if [[ "${1:-}" == image && "${2:-}" == inspect ]]; then
  template="${5:-}"
  case "${template}" in
    *RepoDigests*) printf '%s\n' "${base}" ;;
    '{{.Id}}') printf '%s\n' "${controller_id}" ;;
    *org.ctx.release.arch*) printf '%s\n' "${daemon_arch}" ;;
    *org.ctx.release.base-image*) printf '%s\n' "${base}" ;;
    *org.ctx.release.buildx-version*) printf '0.20.1\n' ;;
    *org.ctx.release.docker-version*) printf '27.5.1\n' ;;
    *org.ctx.release.role*) printf 'ctx-public-bazel-controller\n' ;;
    *org.ctx.release.ubuntu-snapshot*) printf '20260701T000000Z\n' ;;
    *) exit 2 ;;
  esac
  exit 0
fi
if [[ "${1:-}" != run ]]; then
  exit 2
fi
if [[ "$*" == *controller-preflight* ]]; then
  version="${FAKE_CONTROLLER_OS_VERSION:-22.04}"
  arch="${FAKE_CONTROLLER_ARCH:-${daemon_arch}}"
  controller_daemon_id="${FAKE_CONTROLLER_DAEMON_ID:-${daemon_id}}"
  canonical_platform="${@: -1}"
  evidence=$'Linux\t'"${arch}"$'\t'"${arch}"$'\t0\tuname\tgeneric\tnone\tpresent\t1'
  os=$'ubuntu\t'"${version}"$'\tunknown'
  authority="$(
    "${FAKE_SOURCE_ROOT}/scripts/public-cli-runtime-authority.sh" \
      "${canonical_platform}" Linux "${arch}" passed "${arch}" 0 generic none present 1 \
      "" ubuntu "${version}" unknown
  )"
  printf 'evidence\t%s\nos\t%s\nauthority\t%s\n' \
    "${evidence}" "${os}" "${authority}"
  if [[ "${arch}" == aarch64 ]]; then
    docker_sha=3e2d3307e386e59268ab1b17c195f5f224b7c616f06c553d22c0d10f90e5e618
    buildx_sha=f7d867e9f1a3c00b32dd580f56594e229df05e3fb1b083b7099c91c2e7d2ce1e
    zstd_sha=50eed4c67aef71f5a33e82df66788f5415840c66827b6ef2fdf799a046ad59de
  else
    docker_sha=242c7a8de606afba2acada7c7af00d77f92c3601678b2f3a60911b49a892c722
    buildx_sha=8c38f60308a895fa570f1410e453c5de11aafd65a99fa99965d96d24b6225a78
    zstd_sha=d304445daa7e6429293dc02035063b7993fb6a489ee90d8851bff497952836dc
  fi
  printf '%s\n' \
    $'docker_client_version\tDocker version 27.5.1, build pinned' \
    "docker_client_sha"$'\t'"${docker_sha}" \
    $'buildx_version\tgithub.com/docker/buildx v0.20.1 pinned' \
    "buildx_sha"$'\t'"${buildx_sha}" \
    $'zstd_version\t*** zstd command line interface 64-bits v1.4.8, fixture ***' \
    "zstd_sha"$'\t'"${zstd_sha}" \
    "daemon"$'\t'"${arch}"$'\t'"29.1.3"$'\t'"${controller_daemon_id}" \
    "socket"$'\t'"$(stat -c $'%d\t%i\t%f' "${FAKE_SOCKET_PATH}")"
  exit 0
fi
if [[ "$*" != *scripts/release/build-linux-bazel-release.sh* ]]; then
  exit 2
fi

platform=""
commit=""
output=""
symbols=""
while (( $# > 0 )); do
  case "$1" in
    --platform) shift; platform="${1:-}" ;;
    --source-commit) shift; commit="${1:-}" ;;
    --output-dir) shift; output="${1:-}" ;;
    --private-symbols-dir) shift; symbols="${1:-}" ;;
  esac
  shift
done
mkdir "${output}" "${symbols}"
rmdir "${output}"
canonical_platform="${platform}"
[[ "${canonical_platform}" != linux-arm64 ]] || canonical_platform=linux-aarch64
python3 "${FAKE_CANDIDATE_HELPER}" \
  --fixture "${output}" "${canonical_platform}" "${commit}"
if [[ "${FAKE_MUTATE_SOCKET_AFTER:-0}" == 1 ]]; then
  chmod 0777 "${FAKE_SOCKET_PATH}"
fi
EOF
chmod 0755 "${tmp_dir}/fake-bin/docker"

common_env=(
  "PATH=${tmp_dir}/fake-bin:${PATH}"
  "CTX_LINUX_RELEASE_DOCKER_SOCKET=${socket_path}"
  "CTX_LINUX_RELEASE_WORK_ROOT=${tmp_dir}/work"
  "CTX_LINUX_RELEASE_CACHE_ROOT=${tmp_dir}/cache"
  "CTX_OSV_SCANNER=${tmp_dir}/advisory/osv-scanner"
  "CTX_OSV_DATABASE_DIR=${tmp_dir}/advisory/database"
  "CTX_OSV_DATABASE_METADATA=${tmp_dir}/advisory/database-metadata.json"
  "FAKE_DOCKER_LOG=${tmp_dir}/docker.log"
  "FAKE_DOCKER_INFO_COUNT=${tmp_dir}/docker-info-count"
  "FAKE_CANDIDATE_HELPER=${source_root}/scripts/tests/linux-bazel-controller-receipt-test.py"
  "FAKE_SOCKET_PATH=${socket_path}"
  "FAKE_SOURCE_ROOT=${fixture}"
)

env "${common_env[@]}" \
  "${fixture}/scripts/release/run-linux-bazel-release-controller.sh" \
    --platform linux-x64 \
    --source-commit "${source_commit}" \
    --output-dir "${tmp_dir}/outputs/candidate" \
    --private-symbols-dir "${tmp_dir}/outputs/symbols" \
    --controller-receipt "${tmp_dir}/outputs/controller.json"
python3 - "${tmp_dir}/outputs/controller.json" "${source_commit}" <<'PY'
import json
import sys

receipt = json.load(open(sys.argv[1], encoding="utf-8"))
assert receipt["schema_version"] == 1
assert receipt["platform"] == "linux-x64"
assert receipt["source"]["commit"] == sys.argv[2]
assert receipt["controller"]["authority"] == "authoritative"
assert receipt["controller"]["os"]["identity"] == "ubuntu"
assert receipt["controller"]["os"]["version"] == "22.04"
assert receipt["controller"]["evidence"]["emulation"] == "none"
assert receipt["controller"]["docker_client"]["version"].startswith(
    "Docker version 27.5.1,"
)
assert "zstd command line interface" in receipt["controller"]["zstd"]["version"]
assert receipt["controller"]["zstd"]["sha256"] == (
    "d304445daa7e6429293dc02035063b7993fb6a489ee90d8851bff497952836dc"
)
assert receipt["docker"]["daemon"]["before"] == receipt["docker"]["daemon"]["after"]
assert receipt["docker"]["daemon"]["before"]["arch"] == "x86_64"
for scope in ("launcher", "controller"):
    assert receipt["docker"]["socket"][scope]["before"] == receipt["docker"]["socket"][scope]["after"]
assert receipt["artifact"]["path"] == "ctx"
assert len(receipt["candidate_receipts"]["leaves"]) == 17
assert receipt["launcher"]["authority"] in {
    "authoritative",
    "non_authoritative",
}
PY
grep -Fq 'controller-preflight' "${tmp_dir}/docker.log"
grep -Fq 'scripts/release/build-linux-bazel-release.sh' "${tmp_dir}/docker.log"
preflight_log="$(grep -F 'controller-preflight' "${tmp_dir}/docker.log")"
construction_log="$(
  grep -F 'scripts/release/build-linux-bazel-release.sh' "${tmp_dir}/docker.log"
)"
grep -Fq -- '--network none' <<<"${preflight_log}"
if grep -Fq -- '--network none' <<<"${construction_log}"; then
  echo 'controller construction cannot fetch checksum-pinned sidecar inputs' >&2
  exit 1
fi
grep -Fq "TMPDIR=${tmp_dir}/work/ctx-linux-release-controller." \
  "${tmp_dir}/docker.log"
grep -Fq "${tmp_dir}/work:${tmp_dir}/work:rw" "${tmp_dir}/docker.log"
grep -Fq \
  "CTX_ONNXRUNTIME_CACHE_DIR=${tmp_dir}/cache/onnxruntime-sidecar" \
  "${tmp_dir}/docker.log"
while IFS= read -r invocation; do
  [[ "${invocation}" == "--host unix://${socket_path} --config "* ]] || {
    printf 'Docker invocation omitted explicit socket/config binding: %s\n' \
      "${invocation}" >&2
    exit 1
  }
done <"${tmp_dir}/docker.log"

env "${common_env[@]}" FAKE_DAEMON_ARCH=aarch64 \
  "${fixture}/scripts/release/run-linux-bazel-release-controller.sh" \
    --platform linux-arm64 \
    --source-commit "${source_commit}" \
    --output-dir "${tmp_dir}/outputs/arm-candidate" \
    --private-symbols-dir "${tmp_dir}/outputs/arm-symbols" \
    --controller-receipt "${tmp_dir}/outputs/arm-controller.json"
python3 - "${tmp_dir}/outputs/arm-controller.json" "${source_commit}" <<'PY'
import json
import sys

receipt = json.load(open(sys.argv[1], encoding="utf-8"))
assert receipt["platform"] == "linux-aarch64"
assert receipt["source"]["commit"] == sys.argv[2]
assert receipt["artifact"]["path"] == "ctx-linux-aarch64"
assert receipt["docker"]["daemon"]["before"]["arch"] == "aarch64"
names = {record["name"] for record in receipt["candidate_receipts"]["leaves"]}
assert "ctx-linux-aarch64.release-complete.json" in names
assert "ctx-onnxruntime-linux-aarch64.tar.zst.asset.json" in names
assert not any("linux-arm64" in name for name in names)
PY

if env "${common_env[@]}" \
  "${fixture}/scripts/release/run-linux-bazel-release-controller.sh" \
    --platform linux-aarch64 \
    --source-commit "${source_commit}" \
    --output-dir "${tmp_dir}/outputs/alias-candidate" \
    --private-symbols-dir "${tmp_dir}/outputs/alias-symbols" \
    --controller-receipt "${tmp_dir}/outputs/alias-controller.json" \
    >"${tmp_dir}/alias.out" 2>"${tmp_dir}/alias.err"; then
  echo "canonical internal ARM name was accepted as a public target alias" >&2
  exit 1
fi
grep -Fq -- '--platform must be linux-x64 or linux-arm64' "${tmp_dir}/alias.err"

if env "${common_env[@]}" DOCKER_CONTEXT=ambient \
  "${fixture}/scripts/release/run-linux-bazel-release-controller.sh" \
    --platform linux-x64 \
    --source-commit "${source_commit}" \
    --output-dir "${tmp_dir}/outputs/context-candidate" \
    --private-symbols-dir "${tmp_dir}/outputs/context-symbols" \
    --controller-receipt "${tmp_dir}/outputs/context-controller.json" \
    >"${tmp_dir}/context.out" 2>"${tmp_dir}/context.err"; then
  echo "ambient Docker context unexpectedly reached the controller" >&2
  exit 1
fi
grep -Fq 'ambient Docker selector is forbidden: DOCKER_CONTEXT' \
  "${tmp_dir}/context.err"

if env "${common_env[@]}" FAKE_CONTROLLER_DAEMON_ID=other-daemon \
  "${fixture}/scripts/release/run-linux-bazel-release-controller.sh" \
    --platform linux-x64 \
    --source-commit "${source_commit}" \
    --output-dir "${tmp_dir}/outputs/split-candidate" \
    --private-symbols-dir "${tmp_dir}/outputs/split-symbols" \
    --controller-receipt "${tmp_dir}/outputs/split-controller.json" \
    >"${tmp_dir}/split.out" 2>"${tmp_dir}/split.err"; then
  echo "controller/launcher Docker daemon split unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'controller Docker context does not resolve' "${tmp_dir}/split.err"

printf '0\n' >"${tmp_dir}/docker-info-count"
if env "${common_env[@]}" FAKE_MUTATE_DAEMON_AFTER=1 \
  "${fixture}/scripts/release/run-linux-bazel-release-controller.sh" \
    --platform linux-x64 \
    --source-commit "${source_commit}" \
    --output-dir "${tmp_dir}/outputs/mutated-daemon-candidate" \
    --private-symbols-dir "${tmp_dir}/outputs/mutated-daemon-symbols" \
    --controller-receipt "${tmp_dir}/outputs/mutated-daemon-controller.json" \
    >"${tmp_dir}/mutated-daemon.out" 2>"${tmp_dir}/mutated-daemon.err"; then
  echo "Docker daemon mutation unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'Docker daemon authority changed during construction' \
  "${tmp_dir}/mutated-daemon.err"

socket_mode="$(stat -c '%a' "${socket_path}")"
if env "${common_env[@]}" FAKE_MUTATE_SOCKET_AFTER=1 \
  "${fixture}/scripts/release/run-linux-bazel-release-controller.sh" \
    --platform linux-x64 \
    --source-commit "${source_commit}" \
    --output-dir "${tmp_dir}/outputs/mutated-socket-candidate" \
    --private-symbols-dir "${tmp_dir}/outputs/mutated-socket-symbols" \
    --controller-receipt "${tmp_dir}/outputs/mutated-socket-controller.json" \
    >"${tmp_dir}/mutated-socket.out" 2>"${tmp_dir}/mutated-socket.err"; then
  echo "Docker socket mutation unexpectedly passed" >&2
  exit 1
fi
chmod "${socket_mode}" "${socket_path}"
grep -Fq 'Docker Unix socket authority changed during construction' \
  "${tmp_dir}/mutated-socket.err"

if env "${common_env[@]}" FAKE_CONTROLLER_OS_VERSION=24.04 \
  "${fixture}/scripts/release/run-linux-bazel-release-controller.sh" \
    --platform linux-x64 \
    --source-commit "${source_commit}" \
    --output-dir "${tmp_dir}/outputs/wrong-candidate" \
    --private-symbols-dir "${tmp_dir}/outputs/wrong-symbols" \
    --controller-receipt "${tmp_dir}/outputs/wrong-controller.json" \
    >"${tmp_dir}/wrong.out" 2>"${tmp_dir}/wrong.err"; then
  echo "Ubuntu 24 outer controller unexpectedly gained release authority" >&2
  exit 1
fi
grep -Fq 'outer controller is not authoritative' "${tmp_dir}/wrong.err"
[[ ! -e "${tmp_dir}/outputs/wrong-candidate" ]]
[[ ! -e "${tmp_dir}/outputs/wrong-controller.json" ]]

if env "${common_env[@]}" FAKE_DAEMON_ARCH=aarch64 \
  FAKE_CONTROLLER_ARCH=x86_64 \
  "${fixture}/scripts/release/run-linux-bazel-release-controller.sh" \
    --platform linux-arm64 \
    --source-commit "${source_commit}" \
    --output-dir "${tmp_dir}/outputs/wrong-arm-candidate" \
    --private-symbols-dir "${tmp_dir}/outputs/wrong-arm-symbols" \
    --controller-receipt "${tmp_dir}/outputs/wrong-arm-controller.json" \
    >"${tmp_dir}/wrong-arm.out" 2>"${tmp_dir}/wrong-arm.err"; then
  echo "wrong ARM controller architecture unexpectedly gained authority" >&2
  exit 1
fi
grep -Fq 'outer controller is not authoritative' "${tmp_dir}/wrong-arm.err"
[[ ! -e "${tmp_dir}/outputs/wrong-arm-candidate" ]]
[[ ! -e "${tmp_dir}/outputs/wrong-arm-controller.json" ]]

printf 'Linux Bazel release controller test passed\n'
