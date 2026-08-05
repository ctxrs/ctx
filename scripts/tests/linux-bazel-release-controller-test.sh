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
  "${source_root}/scripts/release/write-linux-bazel-controller-receipt.py" \
  "${fixture}/scripts/release"
install -m 0644 \
  "${source_root}/scripts/release/linux-bazel-release-controller.Dockerfile" \
  "${fixture}/scripts/release"
cat >"${fixture}/scripts/release/build-linux-bazel-release.sh" <<'EOF'
#!/usr/bin/env bash
echo "the fake Docker transport must intercept this command" >&2
exit 99
EOF
chmod 0755 "${fixture}/scripts/release/build-linux-bazel-release.sh"
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
controller_id="sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
base="docker.io/library/ubuntu:22.04@sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982"

if [[ "${1:-}" == info ]]; then
  printf 'x86_64\n'
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
    *org.ctx.release.arch*) printf 'x86_64\n' ;;
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
  evidence=$'Linux\tx86_64\tx86_64\t0\tuname\tgeneric\tnone\tpresent\t1'
  os=$'ubuntu\t'"${version}"$'\tunknown'
  authority="$(
    "${FAKE_SOURCE_ROOT}/scripts/public-cli-runtime-authority.sh" \
      linux-x64 Linux x86_64 passed x86_64 0 generic none present 1 \
      "" ubuntu "${version}" unknown
  )"
  printf 'evidence\t%s\nos\t%s\nauthority\t%s\n' \
    "${evidence}" "${os}" "${authority}"
  printf '%s\n' \
    $'docker_client_version\tDocker version 27.5.1, build pinned' \
    $'docker_client_sha\t242c7a8de606afba2acada7c7af00d77f92c3601678b2f3a60911b49a892c722' \
    $'buildx_version\tgithub.com/docker/buildx v0.20.1 pinned' \
    $'buildx_sha\t8c38f60308a895fa570f1410e453c5de11aafd65a99fa99965d96d24b6225a78' \
    $'zstd_version\t*** zstd command line interface 64-bits v1.4.8, fixture ***' \
    $'zstd_sha\td304445daa7e6429293dc02035063b7993fb6a489ee90d8851bff497952836dc' \
    $'daemon\tx86_64\t29.1.3\tfixture-daemon'
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
printf 'fixture artifact\n' >"${output}/ctx"
python3 - "${output}" "${platform}" "${commit}" <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
platform = sys.argv[2]
commit = sys.argv[3]
(root / "ctx.build-info.json").write_text(
    json.dumps(
        {"platform": platform, "source": {"clean": True, "commit": commit}},
        sort_keys=True,
    )
    + "\n",
    encoding="utf-8",
)
(root / f"ctx-{platform}.release-complete.json").write_text(
    json.dumps({"platform": platform, "source_commit": commit}, sort_keys=True)
    + "\n",
    encoding="utf-8",
)
PY
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
assert receipt["docker_daemon"]["arch"] == "x86_64"
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
grep -Fq "TMPDIR=${tmp_dir}/work/controller-tmp" "${tmp_dir}/docker.log"
grep -Fq "${tmp_dir}/work:${tmp_dir}/work:rw" "${tmp_dir}/docker.log"
grep -Fq \
  "CTX_ONNXRUNTIME_CACHE_DIR=${tmp_dir}/cache/onnxruntime-sidecar" \
  "${tmp_dir}/docker.log"

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

printf 'Linux Bazel release controller test passed\n'
