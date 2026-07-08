#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

export CTX_BOOTSTRAP_BAZELISK="${CTX_BOOTSTRAP_BAZELISK:-1}"
export CTX_BAZELISK_VERSION="${CTX_BAZELISK_VERSION:-v1.29.0}"
export CTX_GO_VERSION="${CTX_GO_VERSION:-1.22.12}"
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
    jq \
    nodejs \
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
}

install_go() {
  local go_arch
  case "$(uname -m)" in
    x86_64 | amd64)
      go_arch="amd64"
      ;;
    aarch64 | arm64)
      go_arch="arm64"
      ;;
    *)
      printf 'unsupported Go install architecture: %s\n' "$(uname -m)" >&2
      exit 1
      ;;
  esac

  local go_sha256
  case "${CTX_GO_VERSION}:${go_arch}" in
    1.22.12:amd64)
      go_sha256="4fa4f869b0f7fc6bb1eb2660e74657fbf04cdd290b5aef905585c86051b34d43"
      ;;
    1.22.12:arm64)
      go_sha256="fd017e647ec28525e86ae8203236e0653242722a7436929b1f775744e26278e7"
      ;;
    *)
      printf 'unsupported CTX_GO_VERSION/architecture pair: %s/%s\n' "${CTX_GO_VERSION}" "${go_arch}" >&2
      exit 1
      ;;
  esac

  local go_tarball
  go_tarball="$(mktemp "${TMPDIR:-/tmp}/ctx-go.XXXXXX.tar.gz")"
  curl -fsSL "https://go.dev/dl/go${CTX_GO_VERSION}.linux-${go_arch}.tar.gz" -o "${go_tarball}"
  printf '%s  %s\n' "${go_sha256}" "${go_tarball}" | sha256sum -c -
  rm -rf "${HOME}/.local/go"
  mkdir -p "${HOME}/.local"
  tar -C "${HOME}/.local" -xzf "${go_tarball}"
  rm -f "${go_tarball}"
  export PATH="${HOME}/.local/go/bin:${PATH}"
  go version
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
install_go
install_rust
configure_bazelisk
print_tool_versions
bash scripts/check.sh --mode=ci
