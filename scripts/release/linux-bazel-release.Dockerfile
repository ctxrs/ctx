ARG UBUNTU_IMAGE="docker.io/library/ubuntu:22.04@sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982"
FROM ${UBUNTU_IMAGE} AS common

ARG UBUNTU_IMAGE
ARG UBUNTU_SNAPSHOT="20260701T000000Z"
ARG RELEASE_ARCH

LABEL org.ctx.release.base-image="${UBUNTU_IMAGE}"
LABEL org.ctx.release.arch="${RELEASE_ARCH}"
LABEL org.ctx.release.ubuntu-snapshot="${UBUNTU_SNAPSHOT}"

ENV DEBIAN_FRONTEND=noninteractive

ADD --checksum=sha256:6e8cdcc8c86103acd4fc14649eac62ff2037108389074a7b167567af33c32245 \
  https://snapshot.ubuntu.com/ubuntu/20260701T000000Z/pool/main/c/ca-certificates/ca-certificates_20260601%7e22.04.1_all.deb \
  /tmp/ca-certificates.deb

RUN snapshot="https://snapshot.ubuntu.com/ubuntu/${UBUNTU_SNAPSHOT}" \
  && install -d -m 0755 /tmp/ca-bootstrap /etc/ssl/certs \
  && dpkg-deb -x /tmp/ca-certificates.deb /tmp/ca-bootstrap \
  && cat /tmp/ca-bootstrap/usr/share/ca-certificates/mozilla/*.crt \
    > /etc/ssl/certs/ca-certificates.crt \
  && rm -rf /tmp/ca-bootstrap /tmp/ca-certificates.deb \
  && sed -i \
    -e "s|http://archive.ubuntu.com/ubuntu/|${snapshot}/|g" \
    -e "s|http://security.ubuntu.com/ubuntu/|${snapshot}/|g" \
    /etc/apt/sources.list

FROM common AS builder

ARG UBUNTU_IMAGE
ARG UBUNTU_SNAPSHOT="20260701T000000Z"
ARG GLIBC_BASELINE="2.35"
ARG BAZEL_VERSION="7.7.1"
ARG BAZEL_ARCH
ARG BAZEL_SHA256
ARG RUST_TOOLCHAIN="1.97.1"
ARG RUST_COMMIT="8bab26f4f68e0e26f0bb7960be334d5b520ea452"

LABEL org.ctx.release.bazel-version="${BAZEL_VERSION}"
LABEL org.ctx.release.glibc-baseline="${GLIBC_BASELINE}"
LABEL org.ctx.release.role="ctx-public-bazel-builder"
LABEL org.ctx.release.rust-toolchain="${RUST_TOOLCHAIN}"
LABEL org.ctx.release.rust-commit="${RUST_COMMIT}"

ENV PATH=/opt/ctx/bin:${PATH}

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    bash \
    binutils \
    build-essential \
    ca-certificates \
    curl \
    file \
    git \
    llvm \
    patch \
    perl \
    pkg-config \
    procps \
    python3 \
    python3-tomli \
    unzip \
    xz-utils \
  && rm -rf /var/lib/apt/lists/* \
  && install -d -m 0755 /opt/ctx/bin \
  && case "${BAZEL_ARCH}" in x86_64|arm64) ;; *) exit 1 ;; esac \
  && test -n "${BAZEL_SHA256}" \
  && curl --proto '=https' --tlsv1.2 -fsSL \
    "https://github.com/bazelbuild/bazel/releases/download/${BAZEL_VERSION}/bazel-${BAZEL_VERSION}-linux-${BAZEL_ARCH}" \
    -o /opt/ctx/bin/bazel \
  && printf '%s  %s\n' "${BAZEL_SHA256}" /opt/ctx/bin/bazel \
    | sha256sum --check --strict \
  && chmod 0755 /opt/ctx/bin/bazel \
  && test "$(getconf GNU_LIBC_VERSION)" = "glibc ${GLIBC_BASELINE}" \
  && test "$(bazel --version)" = "bazel ${BAZEL_VERSION}"

FROM common AS runtime

LABEL org.ctx.release.role="runtime"

RUN apt-get update \
  && apt-get install -y --no-install-recommends bash coreutils procps \
  && rm -rf /var/lib/apt/lists/*

FROM common AS inspector

LABEL org.ctx.release.role="inspector"

RUN apt-get update \
  && apt-get install -y --no-install-recommends bash coreutils llvm procps \
  && rm -rf /var/lib/apt/lists/*
