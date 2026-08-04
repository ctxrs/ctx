#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

export CTX_BOOTSTRAP_BAZELISK="${CTX_BOOTSTRAP_BAZELISK:-1}"
export CTX_BAZELISK_VERSION="${CTX_BAZELISK_VERSION:-v1.29.0}"
export CTX_RUST_TOOLCHAIN="${CTX_RUST_TOOLCHAIN:-1.97.1}"

check_args=("$@")
if (( "${#check_args[@]}" == 0 )); then
  check_args=(--mode=ci)
fi

init_buildkite_job_tool_env() {
  if [[ -z "${BUILDKITE_JOB_ID:-}" ]]; then
    return 0
  fi

  local base_tmp job_slug tool_root
  base_tmp="${TMPDIR:-/tmp}"
  job_slug="${BUILDKITE_JOB_ID//[^A-Za-z0-9_.-]/_}"
  tool_root="${CTX_PUBLIC_CI_TOOL_ROOT:-${base_tmp}/ctx-public-ci-${job_slug}}"

  export TMPDIR="${CTX_PUBLIC_CI_TMPDIR:-${tool_root}/tmp}"
  export HOME="${CTX_PUBLIC_CI_HOME:-${tool_root}/home}"
  export CARGO_HOME="${CARGO_HOME:-${tool_root}/cargo-home}"
  export RUSTUP_HOME="${RUSTUP_HOME:-${tool_root}/rustup-home}"
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${tool_root}/cargo-target}"
  export CTX_TOOL_ENV_ROOT="${CTX_TOOL_ENV_ROOT:-${tool_root}/tool-env}"
  export BAZELISK_HOME="${BAZELISK_HOME:-${tool_root}/bazelisk-home}"
  export BAZEL_OUTPUT_USER_ROOT="${BAZEL_OUTPUT_USER_ROOT:-${tool_root}/bazel-output}"
  # Buildkite hosted cache volumes link configured checkout-relative paths to
  # /cache/bkcache. This stable path is the public CI cache-mount contract;
  # without a configured volume it remains a safe job-local repository cache.
  export CTX_PUBLIC_CI_REPOSITORY_CACHE="${CTX_PUBLIC_CI_REPOSITORY_CACHE:-${repo_root}/.buildkite-cache/bazel-repository}"
  export CTX_BAZEL_REPOSITORY_CACHE="${CTX_BAZEL_REPOSITORY_CACHE:-${CTX_PUBLIC_CI_REPOSITORY_CACHE}}"
  mkdir -p \
    "${TMPDIR}" \
    "${HOME}" \
    "${CARGO_HOME}" \
    "${RUSTUP_HOME}" \
    "${CARGO_TARGET_DIR}" \
    "${CTX_TOOL_ENV_ROOT}" \
    "${BAZELISK_HOME}" \
    "${BAZEL_OUTPUT_USER_ROOT}" \
    "${CTX_BAZEL_REPOSITORY_CACHE}"
  printf 'Buildkite job tool root: %s\n' "${tool_root}"
  printf 'Buildkite Bazel repository cache: %s\n' "${CTX_BAZEL_REPOSITORY_CACHE}"
}

run_apt_get() {
  if command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  else
    "$@"
  fi
}

install_ubuntu_tools() {
  local required_packages=(
    build-essential \
    ca-certificates \
    curl \
    default-jdk-headless \
    dotnet-sdk-8.0 \
    git \
    jq \
    nodejs \
    npm \
    openssl \
    pkg-config \
    python3 \
    python3-build \
    python3-pip \
    python3-venv \
    ripgrep \
    ruby \
    unzip \
    zip
  )
  local missing_packages=()
  local package
  for package in "${required_packages[@]}"; do
    if ! dpkg-query -W -f='${Status}\n' "${package}" 2>/dev/null \
      | grep -Fqx 'install ok installed'; then
      missing_packages+=("${package}")
    fi
  done

  if (( "${#missing_packages[@]}" == 0 )); then
    printf 'Buildkite hosted Linux tool packages already installed\n'
    return 0
  fi

  command -v apt-get >/dev/null 2>&1 || {
    printf 'apt-get is required to install missing Buildkite tools: %s\n' \
      "${missing_packages[*]}" >&2
    exit 127
  }

  printf 'Installing missing Buildkite tool packages: %s\n' "${missing_packages[*]}"
  run_apt_get apt-get -o DPkg::Lock::Timeout=300 update
  run_apt_get env DEBIAN_FRONTEND=noninteractive apt-get \
    -o DPkg::Lock::Timeout=300 install -y --no-install-recommends \
    "${missing_packages[@]}"
}

configure_bazelisk() {
  mkdir -p "${HOME}/.local/bin"
  printf 'common --repository_cache=%s\n' "${CTX_BAZEL_REPOSITORY_CACHE}" > "${HOME}/.bazelrc"

  # shellcheck source=scripts/ci-common.sh
  source scripts/ci-common.sh
  bazelisk_path="$(ctx_bootstrap_bazelisk)"
  ln -sf "${bazelisk_path}" "${HOME}/.local/bin/bazelisk"
  ln -sf "${bazelisk_path}" "${HOME}/.local/bin/bazel"
  export PATH="${HOME}/.local/bin:${PATH}"
  bazelisk version
}

print_tool_versions() {
  bazelisk version
  python3 --version
  node --version
  npm --version
  javac -version
  java -version
  dotnet --info
  ruby --version
  jq --version
  rg --version
  openssl version
  zip --version
}

init_buildkite_job_tool_env
install_ubuntu_tools
configure_bazelisk
print_tool_versions
bash scripts/check-sdks.sh --groups=contracts,typescript,python,go,jvm,dotnet --required-groups=contracts,typescript,python,go,jvm,dotnet
# Rust SDK compilation and tests remain authoritative native targets in every
# check.sh mode; the direct gate above owns the other Linux SDK toolchains,
# including the Linux-specific .NET process-tree implementation.
bash scripts/check.sh "${check_args[@]}"
