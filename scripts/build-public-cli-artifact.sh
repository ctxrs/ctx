#!/usr/bin/env bash
set -euo pipefail

ZIG_VERSION="0.14.1"
ZIG_LINUX_X64_URL="https://ziglang.org/download/${ZIG_VERSION}/zig-x86_64-linux-${ZIG_VERSION}.tar.xz"
ZIG_LINUX_X64_SHA256="24aeeec8af16c381934a6cd7d95c807a8cb2cf7df9fa40d359aa884195c4716c"
ZIG_LINUX_AARCH64_URL="https://ziglang.org/download/${ZIG_VERSION}/zig-aarch64-linux-${ZIG_VERSION}.tar.xz"
ZIG_LINUX_AARCH64_SHA256="f7a654acc967864f7a050ddacfaa778c7504a0eca8d2b678839c21eea47c992b"
CARGO_ZIGBUILD_VERSION="0.23.0"
CROSS_VERSION="0.2.5"
LINUX_GLIBC_BASELINE="2.35"
LINUX_RELEASE_IMAGE_UBUNTU="22.04"
LINUX_RELEASE_UBUNTU_DIGEST="sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982"
LINUX_RELEASE_UBUNTU_SNAPSHOT="20260701T000000Z"
LINUX_X64_QEMU_CPU_PROFILE="qemu64"
MACOS_DEPLOYMENT_TARGET="13.0"
RUST_TOOLCHAIN_VERSION="1.97.1"
RUST_TOOLCHAIN_COMMIT="8bab26f4f68e0e26f0bb7960be334d5b520ea452"

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/build-public-cli-artifact.sh PLATFORM

Builds one public ctx CLI binary and stages it under target/public-cli-artifacts.
Platforms: linux-x64, linux-aarch64, macos-arm64, macos-x64, windows-x64, freebsd-x64.
USAGE
}

platform="${1:-}"
if [[ -z "${platform}" || "${platform}" == "-h" || "${platform}" == "--help" ]]; then
  usage
  exit 2
fi
if [[ -n "${CTX_TEST_ONLY_ALLOW_DIRTY_RELEASE_BUILD+x}" ]]; then
  echo "error: forbidden public release environment variable: CTX_TEST_ONLY_ALLOW_DIRTY_RELEASE_BUILD" >&2
  exit 1
fi
if [[ -n "${CTX_LLVM_READOBJ+x}" ]]; then
  echo "error: forbidden public release environment variable: CTX_LLVM_READOBJ" >&2
  exit 1
fi
if [[ -n "${CTX_LLVM_OBJDUMP+x}" ]]; then
  echo "error: forbidden public release environment variable: CTX_LLVM_OBJDUMP" >&2
  exit 1
fi
bash "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/check-linux-release-environment.sh"

case "${platform}" in
  linux-x64)
    target="x86_64-unknown-linux-gnu"
    build_target="${target}"
    binary_name="ctx"
    ;;
  linux-aarch64)
    target="aarch64-unknown-linux-gnu"
    build_target="${target}"
    binary_name="ctx-linux-aarch64"
    ;;
  macos-arm64)
    target="aarch64-apple-darwin"
    build_target="${target}"
    binary_name="ctx-macos-arm64"
    ;;
  macos-x64)
    target="x86_64-apple-darwin"
    build_target="${target}"
    binary_name="ctx-macos-x64"
    ;;
  windows-x64)
    target="x86_64-pc-windows-gnu"
    build_target="${target}"
    binary_name="ctx.exe"
    ;;
  freebsd-x64)
    target="x86_64-unknown-freebsd"
    build_target="${target}"
    binary_name="ctx-freebsd-x64"
    ;;
  *)
    usage
    exit 2
    ;;
esac

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "${root_dir}"

source_commit="$(git rev-parse --verify HEAD)"
if [[ -n "$(git status --porcelain --untracked-files=all)" ]]; then
  echo "error: public release construction requires a clean checkout" >&2
  exit 1
fi
source_clean=true

freebsd_build_strategy=""
if [[ "${platform}" == "freebsd-x64" ]]; then
  freebsd_build_strategy="$(sh scripts/public-cli-freebsd-build-strategy.sh \
    "$(uname -s)" "$(uname -m)")"
fi

inspector_roots=()
freebsd_release_cargo_home=""
cleanup_release_temps() {
  local root
  for root in "${inspector_roots[@]}"; do
    case "$root" in
      "${TMPDIR:-/tmp}"/ctx-public-inspector.*)
        [[ -d "$root" && ! -L "$root" ]] && rm -rf -- "$root"
        ;;
    esac
  done
  case "${freebsd_release_cargo_home}" in
    "${TMPDIR:-/tmp}"/ctx-freebsd-release-cargo.*)
      [[ -d "${freebsd_release_cargo_home}" \
        && ! -L "${freebsd_release_cargo_home}" ]] \
        && rm -rf -- "${freebsd_release_cargo_home}"
      ;;
  esac
}
trap cleanup_release_temps EXIT

stage_inspector_root() {
  local artifact_dir="$1"
  local root
  root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-public-inspector.XXXXXX")"
  scripts/stage-public-cli-inspector-inputs.sh \
    "$root_dir" "$artifact_dir" "$binary_name" "$root" >/dev/null
  inspector_roots+=("$root")
  staged_inspector_root="$root"
}

release_cargo() {
  if [[ "${freebsd_build_strategy}" == "native-freebsd" ]]; then
    cargo "$@"
  else
    cargo "+${RUST_TOOLCHAIN_VERSION}" "$@"
  fi
}

release_rustc_version() {
  if [[ "${freebsd_build_strategy}" == "native-freebsd" ]]; then
    rustc --version
  else
    rustc "+${RUST_TOOLCHAIN_VERSION}" --version
  fi
}

prepare_freebsd_ort_release_source() {
  CARGO_NET_OFFLINE=false release_cargo fetch --locked --target "${target}"
  scripts/prepare-freebsd-ort-release-source.sh \
    "${CARGO_HOME}" \
    "${root_dir}/tools/bazel/patches/ort-freebsd-release-env-order.patch"
  export CARGO_NET_OFFLINE=true
}

ensure_release_rust() {
  if [[ "${freebsd_build_strategy}" == "native-freebsd" ]]; then
    for required_tool in cargo rustc; do
      command -v "${required_tool}" >/dev/null 2>&1 || {
        printf 'error: native FreeBSD release construction requires %s\n' \
          "${required_tool}" >&2
        exit 127
      }
    done
    local native_rust native_cargo target_libdir
    native_rust="$(rustc --version)"
    native_cargo="$(cargo --version)"
    [[ "${native_rust}" == "rustc ${RUST_TOOLCHAIN_VERSION} "* ]] || {
      printf 'error: expected native rustc %s, got %s\n' \
        "${RUST_TOOLCHAIN_VERSION}" "${native_rust}" >&2
      exit 1
    }
    [[ "${native_cargo}" == "cargo ${RUST_TOOLCHAIN_VERSION} "* ]] || {
      printf 'error: expected native cargo %s, got %s\n' \
        "${RUST_TOOLCHAIN_VERSION}" "${native_cargo}" >&2
      exit 1
    }
    target_libdir="$(rustc --print target-libdir --target "${target}")"
    [[ -d "${target_libdir}" ]] || {
      printf 'error: native rustc does not provide target %s\n' "${target}" >&2
      exit 1
    }
    return
  fi

  rustup toolchain install "${RUST_TOOLCHAIN_VERSION}" --profile minimal
  rustup target add --toolchain "${RUST_TOOLCHAIN_VERSION}" "${target}" >/dev/null
  local actual
  actual="$(release_rustc_version)"
  [[ "${actual}" == "rustc ${RUST_TOOLCHAIN_VERSION} "* ]] || {
    printf 'error: expected rustc %s, got %s\n' "${RUST_TOOLCHAIN_VERSION}" "${actual}" >&2
    exit 1
  }
}

zig_host_archive() {
  case "$(uname -m)" in
    x86_64|amd64)
      printf '%s\t%s\t%s\n' \
        "zig-x86_64-linux-${ZIG_VERSION}" \
        "${ZIG_LINUX_X64_URL}" \
        "${ZIG_LINUX_X64_SHA256}"
      ;;
    aarch64|arm64)
      printf '%s\t%s\t%s\n' \
        "zig-aarch64-linux-${ZIG_VERSION}" \
        "${ZIG_LINUX_AARCH64_URL}" \
        "${ZIG_LINUX_AARCH64_SHA256}"
      ;;
    *)
      echo "error: automatic Zig bootstrap does not support Linux $(uname -m)" >&2
      exit 127
      ;;
  esac
}

ensure_zig_for_linux_host() {
  if command -v zig >/dev/null 2>&1 && [[ "$(zig version)" == "${ZIG_VERSION}" ]]; then
    return
  fi

  if [[ "$(uname -s)" != "Linux" ]]; then
    echo "error: zig is required to cross-build ${platform} from $(uname -s)" >&2
    exit 127
  fi

  for required_tool in curl tar; do
    if ! command -v "${required_tool}" >/dev/null 2>&1; then
      echo "error: ${required_tool} is required to bootstrap Zig ${ZIG_VERSION}" >&2
      exit 127
    fi
  done

  IFS=$'\t' read -r zig_archive_dir zig_url zig_sha256 < <(zig_host_archive)
  toolchain_dir="${CTX_PUBLIC_CLI_TOOLCHAIN_DIR:-target/public-cli-toolchain}"
  install_dir="${toolchain_dir}/${zig_archive_dir}"
  if [[ ! -x "${install_dir}/zig" ]]; then
    mkdir -p "${toolchain_dir}"
    archive="${toolchain_dir}/${zig_archive_dir}.tar.xz"
    tmp_archive="${archive}.tmp"
    curl -fsSL "${zig_url}" -o "${tmp_archive}"
    if command -v sha256sum >/dev/null 2>&1; then
      actual_sha="$(sha256sum "${tmp_archive}" | awk '{ print $1 }')"
    elif command -v shasum >/dev/null 2>&1; then
      actual_sha="$(shasum -a 256 "${tmp_archive}" | awk '{ print $1 }')"
    else
      echo "error: sha256sum or shasum is required to verify Zig ${ZIG_VERSION}" >&2
      exit 127
    fi
    if [[ "${actual_sha}" != "${zig_sha256}" ]]; then
      echo "error: Zig ${ZIG_VERSION} checksum mismatch: expected ${zig_sha256}, got ${actual_sha}" >&2
      exit 1
    fi
    mv "${tmp_archive}" "${archive}"
    rm -rf "${install_dir}"
    tar -C "${toolchain_dir}" -xf "${archive}"
  fi
  export PATH="${install_dir}:${PATH}"
  if [[ "$(zig version)" != "${ZIG_VERSION}" ]]; then
    echo "error: expected Zig ${ZIG_VERSION}, got $(zig version)" >&2
    exit 1
  fi
}

ensure_darwin_cross_tools() {
  if ! command -v cargo-zigbuild >/dev/null 2>&1 \
    || [[ "$(cargo-zigbuild --version | sed -n '1p')" != "cargo-zigbuild ${CARGO_ZIGBUILD_VERSION}" ]]; then
    release_cargo install cargo-zigbuild --version "${CARGO_ZIGBUILD_VERSION}" --locked --force
  fi
  [[ "$(cargo-zigbuild --version | sed -n '1p')" == "cargo-zigbuild ${CARGO_ZIGBUILD_VERSION}" ]] || {
    echo "error: cargo-zigbuild ${CARGO_ZIGBUILD_VERSION} is required" >&2
    exit 1
  }
  ensure_zig_for_linux_host
  command -v zig >/dev/null 2>&1 || {
    echo "error: zig is required to cross-build ${platform} from $(uname -s)" >&2
    exit 127
  }
}

artifact_inspector_image_id=""
run_host_artifact_check() {
  if [[ "$(uname -s)" != "Linux" ]]; then
    scripts/check-public-cli-artifact.sh "${platform}" "${out_dir}"
    return
  fi
  case "$(uname -m)" in
    x86_64|amd64) ;;
    *)
      echo "error: cross-target static validation requires the managed Linux x86_64 host" >&2
      exit 1
      ;;
  esac
  command -v docker >/dev/null 2>&1 || {
    echo "error: Docker is required for pinned cross-target static validation" >&2
    exit 127
  }

  local base_image inspector_image actual_base_digest inspector_base_label
  base_image="docker.io/library/ubuntu:${LINUX_RELEASE_IMAGE_UBUNTU}@${LINUX_RELEASE_UBUNTU_DIGEST}"
  inspector_image="ctx-public-cli-cross-inspector:ubuntu-${LINUX_RELEASE_IMAGE_UBUNTU}"
  docker pull --platform linux/amd64 "${base_image}" >/dev/null
  actual_base_digest="$(docker image inspect "${base_image}" --format '{{range .RepoDigests}}{{println .}}{{end}}' \
    | sed -n 's/^.*@\(sha256:[0-9a-f]\{64\}\)$/\1/p' | sort -u)"
  [[ "${actual_base_digest}" == "${LINUX_RELEASE_UBUNTU_DIGEST}" ]] || {
    printf 'error: resolved validation base mismatch: expected %s, got %s\n' \
      "${LINUX_RELEASE_UBUNTU_DIGEST}" "${actual_base_digest:-missing}" >&2
    exit 1
  }
  docker build --platform linux/amd64 \
    --target inspector \
    --provenance=false \
    --build-arg "UBUNTU_IMAGE=${base_image}" \
    --build-arg "UBUNTU_SNAPSHOT=${LINUX_RELEASE_UBUNTU_SNAPSHOT}" \
    -t "${inspector_image}" \
    -f scripts/docker/linux-release.Dockerfile \
    scripts/docker
  artifact_inspector_image_id="$(docker image inspect "${inspector_image}" --format '{{.Id}}')"
  inspector_base_label="$(docker image inspect "${artifact_inspector_image_id}" --format '{{index .Config.Labels "org.ctx.release.base-image"}}')"
  [[ "${inspector_base_label}" == "${base_image}" \
    && "$(docker image inspect "${artifact_inspector_image_id}" --format '{{index .Config.Labels "org.ctx.release.role"}}')" == "inspector" ]] || {
    echo "error: pinned cross-target validation image labels are invalid" >&2
    exit 1
  }
  local inspector_root
  stage_inspector_root "${root_dir}/${out_dir}"
  inspector_root="$staged_inspector_root"
  docker run --rm --platform linux/amd64 \
    --network none \
    --user 65534:65534 \
    --cap-drop ALL \
    --security-opt no-new-privileges \
    --read-only \
    --tmpfs /tmp:rw,nosuid,nodev \
    -e "CTX_PUBLIC_CLI_EXPECTED_VERSION=${version}" \
    -v "${inspector_root}:/work:ro" \
    -w /work \
    "${artifact_inspector_image_id}" \
    bash scripts/check-public-cli-artifact.sh "${platform}" /work/artifacts
}

run_linux_container_build() {
  if [[ "$(uname -s)" != "Linux" ]]; then
    echo "error: ${platform} artifacts must be built from Linux" >&2
    exit 1
  fi
  case "${platform}:$(uname -m)" in
    linux-x64:x86_64|linux-x64:amd64|linux-aarch64:aarch64|linux-aarch64:arm64)
      ;;
    *)
      echo "error: ${platform} artifacts must be built on matching Linux, got $(uname -m)" >&2
      exit 1
      ;;
  esac
  if ! command -v docker >/dev/null 2>&1; then
    echo "error: docker is required to build Linux release artifacts" >&2
    exit 127
  fi
  if ! command -v flock >/dev/null 2>&1; then
    echo "error: flock is required to serialize Linux release construction" >&2
    exit 127
  fi

  local construction_lock_fd construction_lock_file
  construction_lock_file="target/public-cli-locks/${platform}.lock"
  mkdir -p "$(dirname "${construction_lock_file}")"
  exec {construction_lock_fd}>"${construction_lock_file}"
  if ! flock -n "${construction_lock_fd}"; then
    printf 'error: another %s release construction is already using the shared output paths\n' \
      "${platform}" >&2
    exit 1
  fi

  local rust_toolchain="${RUST_TOOLCHAIN_VERSION}"
  local base_image="docker.io/library/ubuntu:${LINUX_RELEASE_IMAGE_UBUNTU}@${LINUX_RELEASE_UBUNTU_DIGEST}"
  local builder_image="ctx-public-cli-linux:${platform}-builder-rust-${rust_toolchain}-ubuntu-${LINUX_RELEASE_IMAGE_UBUNTU}"
  local runtime_image="ctx-public-cli-linux:${platform}-runtime-ubuntu-${LINUX_RELEASE_IMAGE_UBUNTU}"
  local inspector_image="ctx-public-cli-linux:${platform}-inspector-ubuntu-${LINUX_RELEASE_IMAGE_UBUNTU}"
  local out_dir="${CTX_PUBLIC_CLI_ARTIFACT_DIR:-target/public-cli-artifacts}"
  local final_target_dir="${CARGO_TARGET_DIR:-target/public-cli-linux/${platform}}"
  local prepared_dir="target/public-cli-prepared/${platform}"
  local docker_platform
  case "${platform}" in
    linux-x64) docker_platform="linux/amd64" ;;
    linux-aarch64) docker_platform="linux/arm64" ;;
  esac

  for release_path in "${out_dir}" "${final_target_dir}" "${prepared_dir}"; do
    case "${release_path}" in
      /*|..|../*|*/../*|*/..)
        echo "error: Linux release paths must stay under the checkout: ${release_path}" >&2
        exit 1
        ;;
    esac
  done

  local version
  version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; data=json.load(sys.stdin); print(next(pkg["version"] for pkg in data["packages"] if pkg["name"] == "ctx"))')"
  if [[ -n "$(git status --porcelain --untracked-files=all)" ]]; then
    echo "error: Linux release construction requires a clean checkout" >&2
    exit 1
  fi
  source_clean=true

  rm -rf "${prepared_dir}" "${final_target_dir}"
  mkdir -p "${out_dir}" "${prepared_dir}" "${final_target_dir}"
  rm -f \
    "${out_dir}/${binary_name}" \
    "${out_dir}/${binary_name}.sha256" \
    "${out_dir}/${binary_name}.version" \
    "${out_dir}/${binary_name}.expected-version" \
    "${out_dir}/${binary_name}.build-info.json"

  docker pull --platform "${docker_platform}" "${base_image}" >/dev/null
  local actual_base_digest
  actual_base_digest="$(docker image inspect "${base_image}" --format '{{range .RepoDigests}}{{println .}}{{end}}' \
    | sed -n 's/^.*@\(sha256:[0-9a-f]\{64\}\)$/\1/p' | sort -u)"
  if [[ "${actual_base_digest}" != "${LINUX_RELEASE_UBUNTU_DIGEST}" ]]; then
    printf 'error: resolved Ubuntu image mismatch: expected %s, got %s\n' \
      "${LINUX_RELEASE_UBUNTU_DIGEST}" "${actual_base_digest:-missing}" >&2
    exit 1
  fi

  docker build --platform "${docker_platform}" \
    --target builder \
    --provenance=false \
    --build-arg "UBUNTU_IMAGE=${base_image}" \
    --build-arg "UBUNTU_SNAPSHOT=${LINUX_RELEASE_UBUNTU_SNAPSHOT}" \
    --build-arg "GLIBC_BASELINE=${LINUX_GLIBC_BASELINE}" \
    --build-arg "RUST_TOOLCHAIN=${rust_toolchain}" \
    --build-arg "RUST_COMMIT=${RUST_TOOLCHAIN_COMMIT}" \
    -t "${builder_image}" \
    -f scripts/docker/linux-release.Dockerfile \
    scripts/docker
  docker build --platform "${docker_platform}" \
    --target runtime \
    --provenance=false \
    --build-arg "UBUNTU_IMAGE=${base_image}" \
    --build-arg "UBUNTU_SNAPSHOT=${LINUX_RELEASE_UBUNTU_SNAPSHOT}" \
    -t "${runtime_image}" \
    -f scripts/docker/linux-release.Dockerfile \
    scripts/docker
  docker build --platform "${docker_platform}" \
    --target inspector \
    --provenance=false \
    --build-arg "UBUNTU_IMAGE=${base_image}" \
    --build-arg "UBUNTU_SNAPSHOT=${LINUX_RELEASE_UBUNTU_SNAPSHOT}" \
    -t "${inspector_image}" \
    -f scripts/docker/linux-release.Dockerfile \
    scripts/docker
  local builder_base_label builder_glibc_label builder_image_id builder_recipe_sha256
  local builder_rust_commit_label builder_rust_label builder_snapshot_label
  local runtime_base_label runtime_image_id inspector_base_label inspector_image_id
  builder_base_label="$(docker image inspect "${builder_image}" --format '{{index .Config.Labels "org.ctx.release.base-image"}}')"
  builder_snapshot_label="$(docker image inspect "${builder_image}" --format '{{index .Config.Labels "org.ctx.release.ubuntu-snapshot"}}')"
  builder_glibc_label="$(docker image inspect "${builder_image}" --format '{{index .Config.Labels "org.ctx.release.glibc-baseline"}}')"
  builder_rust_label="$(docker image inspect "${builder_image}" --format '{{index .Config.Labels "org.ctx.release.rust-toolchain"}}')"
  builder_rust_commit_label="$(docker image inspect "${builder_image}" --format '{{index .Config.Labels "org.ctx.release.rust-commit"}}')"
  builder_image_id="$(docker image inspect "${builder_image}" --format '{{.Id}}')"
  runtime_base_label="$(docker image inspect "${runtime_image}" --format '{{index .Config.Labels "org.ctx.release.base-image"}}')"
  runtime_image_id="$(docker image inspect "${runtime_image}" --format '{{.Id}}')"
  inspector_base_label="$(docker image inspect "${inspector_image}" --format '{{index .Config.Labels "org.ctx.release.base-image"}}')"
  inspector_image_id="$(docker image inspect "${inspector_image}" --format '{{.Id}}')"
  builder_recipe_sha256="$(sha256sum scripts/docker/linux-release.Dockerfile | awk '{ print $1 }')"
  if [[ "${builder_base_label}" != "${base_image}" ]]; then
    printf 'error: Linux builder base label mismatch: expected %s, got %s\n' \
      "${base_image}" "${builder_base_label:-missing}" >&2
    exit 1
  fi
  if [[ "${builder_snapshot_label}" != "${LINUX_RELEASE_UBUNTU_SNAPSHOT}" \
    || "${builder_glibc_label}" != "${LINUX_GLIBC_BASELINE}" \
    || "${builder_rust_label}" != "${rust_toolchain}" \
    || "${builder_rust_commit_label}" != "${RUST_TOOLCHAIN_COMMIT}" ]]; then
    echo "error: Linux builder snapshot, GLIBC, or Rust labels differ from the release contract" >&2
    exit 1
  fi
  if [[ "${runtime_base_label}" != "${base_image}" || "${inspector_base_label}" != "${base_image}" ]]; then
    printf 'error: Linux runtime/inspector base label mismatch: expected %s\n' "${base_image}" >&2
    exit 1
  fi
  if [[ "$(docker image inspect "${builder_image_id}" --format '{{index .Config.Labels "org.ctx.release.role"}}')" != "builder" \
    || "$(docker image inspect "${runtime_image_id}" --format '{{index .Config.Labels "org.ctx.release.role"}}')" != "runtime" \
    || "$(docker image inspect "${inspector_image_id}" --format '{{index .Config.Labels "org.ctx.release.role"}}')" != "inspector" ]]; then
    echo "error: Linux release image role labels are invalid" >&2
    exit 1
  fi

  docker run --rm --platform "${docker_platform}" \
    --user "$(id -u):$(id -g)" \
    --cap-drop ALL \
    --security-opt no-new-privileges \
    --read-only \
    --tmpfs /tmp:rw,exec,nosuid,nodev \
    -e HOME=/tmp/home \
    -v "${root_dir}:/work:ro" \
    -v "${root_dir}/${prepared_dir}:/prepared:rw" \
    -w /work \
    "${builder_image_id}" \
    bash scripts/prepare-linux-release-inputs.sh \
      "${platform}" "${target}" /prepared

  docker run --rm --platform "${docker_platform}" \
    --network none \
    --user "$(id -u):$(id -g)" \
    --cap-drop ALL \
    --security-opt no-new-privileges \
    --read-only \
    --tmpfs /tmp:rw,exec,nosuid,nodev \
    -e "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}" \
    -e HOME=/tmp/home \
    -v "${root_dir}:/work:ro" \
    -v "${root_dir}/${prepared_dir}:/prepared:ro" \
    -v "${root_dir}/${final_target_dir}:/release-target:rw" \
    -v "${root_dir}/${out_dir}:/artifacts:rw" \
    -w /work \
    "${builder_image_id}" \
    bash scripts/build-linux-release-offline.sh "${platform}" "${target}"

  local staged_host="${root_dir}/${out_dir}/${binary_name}"
  if [[ ! -f "${root_dir}/scripts/run-native-candidate-smoke.sh" ]]; then
    echo "error: required native candidate smoke helper is missing" >&2
    exit 1
  fi
  local inspector_root
  stage_inspector_root "${root_dir}/${out_dir}"
  inspector_root="$staged_inspector_root"
  docker run --rm --platform "${docker_platform}" \
    --network none \
    --user 65534:65534 \
    --cap-drop ALL \
    --security-opt no-new-privileges \
    --read-only \
    --tmpfs /tmp:rw,nosuid,nodev \
    -e "CTX_PUBLIC_CLI_EXPECTED_VERSION=${version}" \
    -v "${inspector_root}:/work:ro" \
    -w /work \
    "${inspector_image_id}" \
    bash scripts/check-public-cli-artifact.sh "${platform}" /work/artifacts
  docker run --rm --platform "${docker_platform}" \
    --network none \
    --user 65534:65534 \
    --cap-drop ALL \
    --security-opt no-new-privileges \
    --read-only \
    --tmpfs /tmp:rw,nosuid,nodev \
    -e HOME=/tmp/home \
    -v "${inspector_root}:/work:ro" \
    -w /work \
    "${runtime_image_id}" \
    bash -euo pipefail -c \
      'timeout --signal=KILL 120s bash scripts/run-native-candidate-smoke.sh "$1" "$2" "$3" /tmp/native-smoke.json && grep -Fq '"'"'"status":"passed"'"'"' /tmp/native-smoke.json' \
      -- \
      "/work/artifacts/${binary_name}" \
      /work/tests/fixtures/custom-history-jsonl/basic.jsonl \
      "${version}"

  local qemu_version="" qemu_cpu_profile=""
  if [[ "${platform}" == "linux-x64" ]]; then
    qemu_cpu_profile="${LINUX_X64_QEMU_CPU_PROFILE}"
    docker run --rm --platform "${docker_platform}" \
      --network none \
      --user 65534:65534 \
      --cap-drop ALL \
      --security-opt no-new-privileges \
      --read-only \
      --tmpfs /tmp:rw,exec,nosuid,nodev \
      -e HOME=/tmp/home \
      -v "${inspector_root}:/work:ro" \
      -w /work \
      "${inspector_image_id}" \
      bash -euo pipefail -c \
        'printf '\''#!/usr/bin/env bash\nexec qemu-x86_64 -cpu %q %q "$@"\n'\'' "$1" "$3" > /tmp/qemu-ctx
         chmod 0755 /tmp/qemu-ctx
         timeout --signal=KILL 180s bash scripts/run-native-candidate-smoke.sh \
           /tmp/qemu-ctx /work/tests/fixtures/custom-history-jsonl/basic.jsonl "$2" /tmp/qemu-smoke.json
         grep -Fq '\''"status":"passed"'\'' /tmp/qemu-smoke.json' \
        -- "${qemu_cpu_profile}" "${version}" "/work/artifacts/${binary_name}"
    qemu_version="$(docker run --rm --platform "${docker_platform}" "${inspector_image_id}" \
      qemu-x86_64 --version | sed -n '1p')"
  fi

  local rust_version
  rust_version="$(docker run --rm --platform "${docker_platform}" "${builder_image_id}" rustc --version)"
  if [[ "$(git rev-parse --verify HEAD)" != "${source_commit}" ]]; then
    echo "error: source commit changed during Linux release construction" >&2
    exit 1
  fi
  if [[ "${source_clean}" == "true" && -n "$(git status --porcelain --untracked-files=all)" ]]; then
    echo "error: source checkout became dirty during Linux release construction" >&2
    exit 1
  fi
  python3 scripts/write-public-cli-build-info.py \
    --output "${staged_host}.build-info.json" \
    --artifact "${staged_host}" \
    --cargo-lock Cargo.lock \
    --platform "${platform}" \
    --target "${target}" \
    --source-commit "${source_commit}" \
    --source-clean "${source_clean}" \
    --rust-version "${rust_version}" \
    --expected-builder-base "${LINUX_RELEASE_UBUNTU_DIGEST}" \
    --actual-builder-base "${actual_base_digest}" \
    --builder-image-id "${builder_image_id}" \
    --builder-recipe-sha256 "${builder_recipe_sha256}" \
    --runtime-image-id "${runtime_image_id}" \
    --inspector-image-id "${inspector_image_id}" \
    --linux-builder-image "${base_image}" \
    --linux-ubuntu-snapshot "${LINUX_RELEASE_UBUNTU_SNAPSHOT}" \
    --linux-glibc-max "${LINUX_GLIBC_BASELINE}" \
    --linux-rust-toolchain "${rust_toolchain}" \
    --linux-rust-commit "${RUST_TOOLCHAIN_COMMIT}" \
    --linux-rust-sysroot "/opt/rustup/toolchains/${rust_toolchain}-${target}" \
    --qemu-version "${qemu_version}" \
    --qemu-cpu-profile "${qemu_cpu_profile}" \
    --static-status passed \
    --local-runtime-status passed \
    --local-runtime-authority authoritative

  printf 'built %s for %s with verified offline inputs and runtime evidence\n' \
    "${staged_host}" "${platform}"
}

if [[ "${platform}" == linux-* ]]; then
  bash scripts/check-linux-release-environment.sh
  run_linux_container_build
  exit 0
fi

out_dir="${CTX_PUBLIC_CLI_ARTIFACT_DIR:-target/public-cli-artifacts}"
case "${out_dir}" in
  /*|..|../*|*/../*|*/..)
    echo "error: public CLI artifact directory must stay under the checkout: ${out_dir}" >&2
    exit 1
    ;;
esac
mkdir -p "${out_dir}"
rm -f \
  "${out_dir}/${binary_name}" \
  "${out_dir}/${binary_name}.sha256" \
  "${out_dir}/${binary_name}.version" \
  "${out_dir}/${binary_name}.build-info.json"

if [[ "${platform}" != linux-* ]]; then
  ensure_release_rust
fi
if [[ "${platform}" == "freebsd-x64" ]]; then
  freebsd_release_cargo_home="$(
    mktemp -d "${TMPDIR:-/tmp}/ctx-freebsd-release-cargo.XXXXXX"
  )"
  export CARGO_HOME="${freebsd_release_cargo_home}"
  export PATH="${CARGO_HOME}/bin:${PATH}"
fi
version="$(if [[ "${platform}" == linux-* ]]; then cargo metadata --no-deps --format-version 1; else release_cargo metadata --no-deps --format-version 1; fi | python3 -c 'import json,sys; data=json.load(sys.stdin); print(next(pkg["version"] for pkg in data["packages"] if pkg["name"] == "ctx"))')"
if [[ -z "${version}" ]]; then
  echo "error: could not determine ctx package version from Cargo metadata" >&2
  exit 1
fi
echo "building ctx ${version} for ${platform}"

build_target_dir="${CARGO_TARGET_DIR:-target}"

if [[ "${platform}" == macos-* ]]; then
  export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-${MACOS_DEPLOYMENT_TARGET}}"
fi

if [[ "${platform}" == macos-* && "$(uname -s)" != "Darwin" ]]; then
  ensure_darwin_cross_tools
  release_cargo zigbuild -p ctx --release --target "${build_target}" --locked
elif [[ "${platform}" == "freebsd-x64" ]]; then
  case "${freebsd_build_strategy}" in
    native-freebsd)
      prepare_freebsd_ort_release_source
      release_cargo build -p ctx --release --target "${target}" --locked
      scripts/prepare-freebsd-ort-release-source.sh \
        "${CARGO_HOME}" \
        "${root_dir}/tools/bazel/patches/ort-freebsd-release-env-order.patch"
      ;;
    linux-cross)
      if ! command -v cross >/dev/null 2>&1 \
        || [[ "$(cross --version | sed -n '1p')" != "cross ${CROSS_VERSION}" ]]; then
        release_cargo install cross --version "${CROSS_VERSION}" --locked --force
      fi
      [[ "$(cross --version | sed -n '1p')" == "cross ${CROSS_VERSION}" ]] || {
        echo "error: cross ${CROSS_VERSION} is required" >&2
        exit 1
      }
      if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
        export CARGO_TARGET_DIR="target/public-cli-cross/${platform}"
        build_target_dir="${CARGO_TARGET_DIR}"
      fi
      prepare_freebsd_ort_release_source
      RUSTUP_TOOLCHAIN="${RUST_TOOLCHAIN_VERSION}" \
        cross build -p ctx --release --target "${target}" --locked
      scripts/prepare-freebsd-ort-release-source.sh \
        "${CARGO_HOME}" \
        "${root_dir}/tools/bazel/patches/ort-freebsd-release-env-order.patch"
      ;;
    *)
      printf 'error: unsupported FreeBSD build strategy: %s\n' \
        "${freebsd_build_strategy:-missing}" >&2
      exit 1
      ;;
  esac
else
  release_cargo build -p ctx --release --target "${build_target}" --locked
fi

target_binary="${build_target_dir}/${target}/release/ctx"
if [[ ! -f "${target_binary}" && "${build_target}" != "${target}" ]]; then
  target_binary="${build_target_dir}/${build_target}/release/ctx"
fi
if [[ "${platform}" == "windows-x64" ]]; then
  target_binary="${target_binary}.exe"
fi
staged="${out_dir}/${binary_name}"
cp "${target_binary}" "${staged}"
chmod 755 "${staged}"
if [[ "${platform}" == "windows-x64" ]]; then
  printf '%s\n' "${version}" > "${staged}.expected-version"
fi

if [[ "${platform}" == macos-* ]]; then
  macos_signing_mode="${CTX_MACOS_RELEASE_SIGNING:-optional}"
  if [[ "${CTX_PUBLIC_CLI_ARTIFACT_MATRIX:-0}" == "1" ]]; then
    macos_signing_mode=required
  fi
  case "${macos_signing_mode}" in
    required)
      scripts/run-macos-release-signing.sh \
        "${platform}" cli "${staged}" "${out_dir}"
      scripts/verify-macos-signed-cli.sh \
        "${platform}" "${staged}" "${version}" \
        "${out_dir}/ctx-${platform}.signing.json"
      ;;
    optional) ;;
    *)
      printf 'error: CTX_MACOS_RELEASE_SIGNING must be optional or required, got %s\n' \
        "${macos_signing_mode}" >&2
      exit 2
      ;;
  esac
fi

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
  linux-x64|linux-aarch64)
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
  freebsd-x64)
    if [[ "${freebsd_build_strategy}" == "native-freebsd" ]]; then
      "${staged}" --version | tee "${staged}.version"
      grep -Fx "ctx ${version}" "${staged}.version" >/dev/null
    else
      printf 'not run on this host: %s\n' "${platform}" > "${staged}.version"
    fi
    ;;
  *)
    printf 'not run on this host: %s\n' "${platform}" > "${staged}.version"
    ;;
esac

if [[ "${platform}" != linux-* ]]; then
  run_host_artifact_check
fi

if [[ "${platform}" != linux-* ]]; then
  if [[ "$(git rev-parse --verify HEAD)" != "${source_commit}" ]]; then
    echo "error: source commit changed during public release construction" >&2
    exit 1
  fi
  if [[ "${source_clean}" == true && -n "$(git status --porcelain --untracked-files=all)" ]]; then
    echo "error: source checkout became dirty during public release construction" >&2
    exit 1
  fi
  local_runtime_status=not_run
  if [[ "$(tr -d '\r' < "${staged}.version" | tail -n 1)" == "ctx ${version}" ]]; then
    local_runtime_status=passed
  fi
  if [[ "${local_runtime_status}" == passed \
    && ( "${platform}" == macos-* || "${platform}" == "freebsd-x64" ) ]]; then
    native_smoke_result="$(mktemp "${TMPDIR:-/tmp}/ctx-native-smoke.XXXXXX")"
    rm -f "${native_smoke_result}"
    scripts/run-native-candidate-smoke.sh \
      "${staged}" tests/fixtures/custom-history-jsonl/basic.jsonl \
      "${version}" "${native_smoke_result}"
    grep -Fq '"status":"passed"' "${native_smoke_result}"
    rm -f "${native_smoke_result}"
  fi
  IFS=$'\t' read -r \
    host_system host_arch host_native_arch process_translated _native_arch_probe \
    hardware_identity emulation hypervisor evidence_complete \
    < <(scripts/public-cli-host-runtime-evidence.sh)
  local_runtime_authority="$(scripts/public-cli-runtime-authority.sh \
    "${platform}" "${host_system}" "${host_arch}" "${local_runtime_status}" \
    "${host_native_arch}" "${process_translated}" "${hardware_identity}" \
    "${emulation}" "${hypervisor}" "${evidence_complete}" \
    "${CTX_RELEASE_MACOS_X64_KVM_RUNNER_ID:-}")"
  python3 scripts/write-public-cli-build-info.py \
    --output "${staged}.build-info.json" \
    --artifact "${staged}" \
    --cargo-lock Cargo.lock \
    --platform "${platform}" \
    --target "${target}" \
    --source-commit "${source_commit}" \
    --source-clean "${source_clean}" \
    --rust-version "$(release_rustc_version)" \
    --inspector-image-id "${artifact_inspector_image_id}" \
    --static-status passed \
    --local-runtime-status "${local_runtime_status}" \
    --local-runtime-authority "${local_runtime_authority}"
  if [[ "${platform}" == macos-* && "${macos_signing_mode}" == required ]]; then
    scripts/check-macos-release-signing.sh \
      "${platform}" cli "${staged}" "${out_dir}/ctx-${platform}.signing.json"
  fi
fi

printf 'built %s for %s sha256=%s\n' "${staged}" "${platform}" "$(cat "${sha_file}")"
