ARG UBUNTU_IMAGE="docker.io/library/ubuntu:22.04@sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982"
FROM ${UBUNTU_IMAGE}

ARG UBUNTU_IMAGE
ARG UBUNTU_SNAPSHOT="20260701T000000Z"
ARG CONTROLLER_ARCH
ARG DOCKER_ARCH
ARG DOCKER_SHA256
ARG DOCKER_VERSION="27.5.1"
ARG BUILDX_ARCH
ARG BUILDX_SHA256
ARG BUILDX_VERSION="0.20.1"

LABEL org.ctx.release.arch="${CONTROLLER_ARCH}"
LABEL org.ctx.release.base-image="${UBUNTU_IMAGE}"
LABEL org.ctx.release.buildx-version="${BUILDX_VERSION}"
LABEL org.ctx.release.docker-version="${DOCKER_VERSION}"
LABEL org.ctx.release.role="ctx-public-bazel-controller"
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
    /etc/apt/sources.list \
  && apt-get update \
  && apt-get install -y --no-install-recommends \
    bash \
    ca-certificates \
    coreutils \
    curl \
    git \
    python3 \
    util-linux \
    zstd \
  && rm -rf /var/lib/apt/lists/*

RUN case "${DOCKER_ARCH}:${BUILDX_ARCH}" in \
      x86_64:amd64|aarch64:arm64) ;; \
      *) exit 1 ;; \
    esac \
  && curl --proto '=https' --tlsv1.2 -fsSL \
    "https://download.docker.com/linux/static/stable/${DOCKER_ARCH}/docker-${DOCKER_VERSION}.tgz" \
    -o /tmp/docker.tgz \
  && printf '%s  %s\n' "${DOCKER_SHA256}" /tmp/docker.tgz \
    | sha256sum --check --strict \
  && tar -xzf /tmp/docker.tgz -C /tmp \
  && install -m 0755 /tmp/docker/docker /usr/local/bin/docker \
  && install -d -m 0755 /usr/local/lib/docker/cli-plugins \
  && curl --proto '=https' --tlsv1.2 -fsSL \
    "https://github.com/docker/buildx/releases/download/v${BUILDX_VERSION}/buildx-v${BUILDX_VERSION}.linux-${BUILDX_ARCH}" \
    -o /usr/local/lib/docker/cli-plugins/docker-buildx \
  && printf '%s  %s\n' \
    "${BUILDX_SHA256}" /usr/local/lib/docker/cli-plugins/docker-buildx \
    | sha256sum --check --strict \
  && chmod 0755 /usr/local/lib/docker/cli-plugins/docker-buildx \
  && rm -rf /tmp/docker /tmp/docker.tgz \
  && test "$(docker --version | sed -n 's/^Docker version \([^,]*\),.*$/\1/p')" \
    = "${DOCKER_VERSION}" \
  && docker buildx version | grep -F "v${BUILDX_VERSION}" >/dev/null \
  && zstd --version | grep -F "v1.4.8," >/dev/null \
  && test "$(getconf GNU_LIBC_VERSION)" = "glibc 2.35"
