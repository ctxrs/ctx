ARG UBUNTU_IMAGE="docker.io/library/ubuntu:22.04@sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982"
FROM ${UBUNTU_IMAGE} AS builder

ARG UBUNTU_IMAGE
ARG UBUNTU_SNAPSHOT="20260701T000000Z"
ARG RUST_TOOLCHAIN="1.97.1"
ARG RUSTUP_VERSION="1.28.2"
ARG RUSTUP_INIT_X86_64_SHA256="20a06e644b0d9bd2fbdbfd52d42540bdde820ea7df86e92e533c073da0cdd43c"
ARG RUSTUP_INIT_AARCH64_SHA256="e3853c5a252fca15252d07cb23a1bdd9377a8c6f3efa01531109281ae47f841c"

LABEL org.ctx.release.base-image="${UBUNTU_IMAGE}"
LABEL org.ctx.release.ubuntu-snapshot="${UBUNTU_SNAPSHOT}"
LABEL org.ctx.release.rust-toolchain="${RUST_TOOLCHAIN}"
LABEL org.ctx.release.rustup-version="${RUSTUP_VERSION}"
LABEL org.ctx.release.role="builder"

ENV DEBIAN_FRONTEND=noninteractive
ENV RUSTUP_HOME=/opt/rustup
ENV CARGO_HOME=/opt/cargo
ENV PATH=/opt/cargo/bin:${PATH}

RUN snapshot="https://snapshot.ubuntu.com/ubuntu/${UBUNTU_SNAPSHOT}" \
  && sed -i \
    -e "s|http://archive.ubuntu.com/ubuntu/|${snapshot}/|g" \
    -e "s|http://security.ubuntu.com/ubuntu/|${snapshot}/|g" \
    -e "s|http://ports.ubuntu.com/ubuntu-ports/|${snapshot}/|g" \
    /etc/apt/sources.list \
  # The minimal Ubuntu base has no CA bundle. APT still authenticates the
  # snapshot's signed InRelease metadata and package hashes during bootstrap.
  && printf '%s\n' 'Acquire::https::Verify-Peer "false";' > /etc/apt/apt.conf.d/00snapshot-ca-bootstrap \
  && apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates \
  && rm /etc/apt/apt.conf.d/00snapshot-ca-bootstrap \
  && rm -rf /var/lib/apt/lists/* \
  && apt-get update \
  && apt-get install -y --no-install-recommends \
    bash \
    binutils \
    build-essential \
    curl \
    file \
    git \
    pkg-config \
    python3 \
    xz-utils \
  && rm -rf /var/lib/apt/lists/*

RUN case "$(dpkg --print-architecture)" in \
      amd64) \
        rustup_target="x86_64-unknown-linux-gnu"; \
        rustup_sha256="${RUSTUP_INIT_X86_64_SHA256}" \
        ;; \
      arm64) \
        rustup_target="aarch64-unknown-linux-gnu"; \
        rustup_sha256="${RUSTUP_INIT_AARCH64_SHA256}" \
        ;; \
      *) \
        echo "unsupported Linux release builder architecture: $(dpkg --print-architecture)" >&2; \
        exit 1 \
        ;; \
    esac \
  && rustup_url="https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${rustup_target}/rustup-init" \
  && curl --proto '=https' --tlsv1.2 -fsSL "${rustup_url}" -o /tmp/rustup-init \
  && printf '%s  %s\n' "${rustup_sha256}" /tmp/rustup-init | sha256sum --check --strict \
  && chmod 0755 /tmp/rustup-init \
  && /tmp/rustup-init -y \
    --no-modify-path \
    --profile minimal \
    --default-toolchain "${RUST_TOOLCHAIN}" \
  && rm /tmp/rustup-init \
  && rustup target list --installed | grep -Fx "${rustup_target}" \
  && chmod -R a+rX /opt/cargo /opt/rustup

ENV CARGO_HOME=/tmp/cargo-home

FROM ${UBUNTU_IMAGE} AS runtime

ARG UBUNTU_IMAGE
ARG UBUNTU_SNAPSHOT="20260701T000000Z"

LABEL org.ctx.release.base-image="${UBUNTU_IMAGE}"
LABEL org.ctx.release.ubuntu-snapshot="${UBUNTU_SNAPSHOT}"
LABEL org.ctx.release.role="runtime"

# The pinned Ubuntu base already contains the POSIX shell/coreutils/procps
# surface used by the native smoke. Keep ordinary execution free of compilers,
# LLVM, and emulators.
RUN for tool in bash env ps awk sed grep comm mktemp sort dirname uname; do \
      command -v "${tool}" >/dev/null || { echo "runtime tool missing: ${tool}" >&2; exit 1; }; \
    done

FROM ${UBUNTU_IMAGE} AS inspector

ARG UBUNTU_IMAGE
ARG UBUNTU_SNAPSHOT="20260701T000000Z"

LABEL org.ctx.release.base-image="${UBUNTU_IMAGE}"
LABEL org.ctx.release.ubuntu-snapshot="${UBUNTU_SNAPSHOT}"
LABEL org.ctx.release.role="inspector"

ENV DEBIAN_FRONTEND=noninteractive

# Keep static inspection and representative-CPU emulation out of both the
# compiler image and the ordinary runtime image.
RUN snapshot="https://snapshot.ubuntu.com/ubuntu/${UBUNTU_SNAPSHOT}" \
  && sed -i \
    -e "s|http://archive.ubuntu.com/ubuntu/|${snapshot}/|g" \
    -e "s|http://security.ubuntu.com/ubuntu/|${snapshot}/|g" \
    -e "s|http://ports.ubuntu.com/ubuntu-ports/|${snapshot}/|g" \
    /etc/apt/sources.list \
  && printf '%s\n' 'Acquire::https::Verify-Peer "false";' > /etc/apt/apt.conf.d/00snapshot-ca-bootstrap \
  && apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates \
  && rm /etc/apt/apt.conf.d/00snapshot-ca-bootstrap \
  && rm -rf /var/lib/apt/lists/* \
  && apt-get update \
  && apt-get install -y --no-install-recommends llvm procps qemu-user \
  && rm -rf /var/lib/apt/lists/*
