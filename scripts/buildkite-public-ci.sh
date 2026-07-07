#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

export CTX_BOOTSTRAP_BAZELISK="${CTX_BOOTSTRAP_BAZELISK:-1}"
export CTX_BAZELISK_VERSION="${CTX_BAZELISK_VERSION:-v1.29.0}"
export CTX_RUST_TOOLCHAIN="${CTX_RUST_TOOLCHAIN:-1.86.0}"

run_apt_get() {
  if command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  else
    "$@"
  fi
}

install_ubuntu_tools() {
  command -v apt-get >/dev/null 2>&1 || {
    printf 'apt-get is required on the Buildkite hosted Linux image\n' >&2
    exit 127
  }

  run_apt_get apt-get update
  run_apt_get env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    build-essential \
    ca-certificates \
    curl \
    default-jdk-headless \
    git \
    golang-go \
    jq \
    nodejs \
    openssl \
    pkg-config \
    python3 \
    python3-build \
    python3-pip \
    ripgrep \
    ruby \
    unzip \
    zip
}

install_rust() {
  export CARGO_HOME="${CARGO_HOME:-${HOME}/.cargo}"
  export RUSTUP_HOME="${RUSTUP_HOME:-${HOME}/.rustup}"
  export PATH="${CARGO_HOME}/bin:${PATH}"

  if ! command -v rustup >/dev/null 2>&1; then
    rustup_installer="$(mktemp "${TMPDIR:-/tmp}/ctx-rustup-init.XXXXXX")"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o "${rustup_installer}"
    sh "${rustup_installer}" -y --profile minimal --default-toolchain none
    rm -f "${rustup_installer}"
  fi

  rustup toolchain install "${CTX_RUST_TOOLCHAIN}" --profile minimal --component rustfmt --component clippy
  rustup default "${CTX_RUST_TOOLCHAIN}"
}

configure_bazelisk() {
  mkdir -p "${HOME}/.cache/bazel-repository" "${HOME}/.local/bin"
  printf 'common --repository_cache=%s\n' "${HOME}/.cache/bazel-repository" > "${HOME}/.bazelrc"

  # shellcheck source=scripts/ci-common.sh
  source scripts/ci-common.sh
  bazelisk_path="$(ctx_bootstrap_bazelisk)"
  ln -sf "${bazelisk_path}" "${HOME}/.local/bin/bazelisk"
  ln -sf "${bazelisk_path}" "${HOME}/.local/bin/bazel"
  export PATH="${HOME}/.local/bin:${PATH}"
  bazelisk version
}

print_tool_versions() {
  rustc --version
  cargo --version
  cargo fmt --version
  cargo clippy --version
  bazelisk version
  python3 --version
  node --version
  npm --version
  go version
  javac -version
  java -version
  ruby --version
  jq --version
  rg --version
  openssl version
  zip --version
}

install_ubuntu_tools
install_rust
configure_bazelisk
print_tool_versions
bash scripts/check.sh --mode=ci
