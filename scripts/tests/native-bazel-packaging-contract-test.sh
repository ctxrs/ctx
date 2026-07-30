#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  source_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  source_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fi

wrapper="${source_root}/scripts/release/build-linux-bazel-release.sh"
dogfood_wrapper="${source_root}/scripts/release/build-linux-x64-bazel-dogfood.sh"
recipe="${source_root}/scripts/release/linux-bazel-release.Dockerfile"
pipeline="${source_root}/.buildkite/pipeline.yml"
matrix="${source_root}/contracts/release-targets-v1.json"
staging="${source_root}/scripts/stage-github-release-assets.sh"
packager="${source_root}/scripts/package-public-cli-bazel-release.sh"
release_routes="${source_root}/tools/bazel/release_routes.bzl"

for required in \
  'route_target=//:ctx_release_linux_x64' \
  'route_target=//:ctx_release_linux_arm64' \
  'docker_platform=linux/amd64' \
  'docker_platform=linux/arm64' \
  'requires a native ${expected_host_arch} host' \
  'requires a native ${expected_host_arch} Docker daemon' \
  'emulation is diagnostic only' \
  'scripts/public-cli-host-runtime-evidence.sh' \
  'scripts/public-cli-runtime-authority.sh' \
  '/build/release-input/${CTX_RELEASE_BINARY_NAME}.build-info.json' \
  '--private-symbols-dir' \
  'scripts/release/detached-debug-symbols.py prepare' \
  '/release-symbol-output/bundle' \
  '--network none' \
  '--lockfile_mode=error' \
  '${CTX_PUBLIC_TARGET_BINARY}.cdx.json.sha256' \
  '${CTX_PUBLIC_TARGET_BINARY}.third-party-notices.txt.sha256' \
  '${CTX_PUBLIC_TARGET_BINARY}.size.json' \
  '${CTX_PUBLIC_TARGET_BINARY}.candidate.json' \
  'd7aedc8565ed47b6231badb80b09f034e389c5f2b1c2ac2c55406f7c661d8b88' \
  'c97f02133adce63f0c28678ac1f21d65fa8255c80429b588aeeba8a1fac6202b'; do
  grep -Fq -- "${required}" "${wrapper}" || {
    printf 'native Linux Bazel wrapper missing contract: %s\n' "${required}" >&2
    exit 1
  }
done

for required in \
  'detached-debug-symbols.py" prepare' \
  'detached-debug-symbols.py" finalize' \
  'public CLI private debug symbols:'; do
  grep -Fq -- "${required}" "${packager}" || {
    printf 'native release packager omits detached symbols: %s\n' \
      "${required}" >&2
    exit 1
  }
done
if grep -Fq 'private-debug-symbols' "${pipeline}"; then
  echo "private debug symbols must not be uploaded as Buildkite artifacts" >&2
  exit 1
fi

for required in \
  '--declared-advisory-gate-runfile' \
  '--declared-sbom-tool-runfile' \
  'script = "//:dependency_advisory_gate"' \
  'sbom_tool = "//:release_sbom"' \
  'export RUNFILES_DIR="${{runfiles_root}}"' \
  'export RUNFILES_MANIFEST_FILE="${{manifest}}"'; do
  grep -Fq -- "${required}" "${release_routes}" || {
    printf 'native release launcher does not forward runfiles: %s\n' \
      "${required}" >&2
    exit 1
  }
done

for required in \
  '"${sbom_tool}" generate' \
  '"${sbom_tool}" verify' \
  '"${sbom_tool}" verify-bundle'; do
  grep -Fq -- "${required}" "${packager}" || {
    printf 'native release packager bypasses declared SBOM tool: %s\n' \
      "${required}" >&2
    exit 1
  }
done
if grep -Fq 'python3 -I "${repo_root}/scripts/release-sbom.py"' "${packager}"; then
  echo "native release packager invokes the host-Python SBOM script" >&2
  exit 1
fi

if grep -Eq 'cargo (build|zigbuild)|qemu-' "${wrapper}"; then
  echo "native Linux Bazel wrapper contains Cargo construction or emulation" >&2
  exit 1
fi

for required in \
  'bazel_binary_arch=x86_64' \
  'bazel_binary_sha256=c97f02133adce63f0c28678ac1f21d65fa8255c80429b588aeeba8a1fac6202b' \
  '--build-arg "BAZEL_ARCH=${bazel_binary_arch}"' \
  '--build-arg "BAZEL_SHA256=${bazel_binary_sha256}"' \
  '--build-arg "RELEASE_ARCH=x86_64"' \
  'CTX_OSV_SCANNER=/release-advisory/osv-scanner' \
  'CTX_OSV_DATABASE_DIR=/release-advisory/database' \
  'CTX_OSV_DATABASE_METADATA=/release-advisory/database-metadata.json'; do
  grep -Fq -- "${required}" "${dogfood_wrapper}" || {
    printf 'Linux x64 dogfood wrapper missing architecture pin: %s\n' \
      "${required}" >&2
    exit 1
  }
done

for release_wrapper in "${wrapper}" "${dogfood_wrapper}"; do
  for required in \
    'CTX_OSV_SCANNER must be an executable non-symlink file' \
    'CTX_OSV_DATABASE_DIR must be a non-symlink directory' \
    'CTX_OSV_DATABASE_METADATA must be a regular non-symlink file' \
    '${osv_scanner_input}:/release-advisory/osv-scanner:ro' \
    '${osv_database_input}:/release-advisory/database:ro' \
    '${osv_metadata_input}:/release-advisory/database-metadata.json:ro'; do
    grep -Fq -- "${required}" "${release_wrapper}" || {
      printf 'native Linux Bazel wrapper omits advisory input: %s\n' \
        "${required}" >&2
      exit 1
    }
  done
done

for required in \
  'ARG BAZEL_ARCH' \
  'ARG BAZEL_SHA256' \
  'ARG RELEASE_ARCH' \
  'org.ctx.release.arch' \
  'bazel-${BAZEL_VERSION}-linux-${BAZEL_ARCH}' \
  '"${BAZEL_SHA256}"'; do
  grep -Fq -- "${required}" "${recipe}" || {
    printf 'native Linux Bazel recipe missing architecture pin: %s\n' \
      "${required}" >&2
    exit 1
  }
done

if grep -Fq 'build-public-cli-artifact.sh' "${pipeline}"; then
  echo "Buildkite release candidates still use the Cargo constructor" >&2
  exit 1
fi
for required in \
  'scripts/release/build-linux-bazel-release.sh' \
  '--platform linux-x64' \
  '--platform linux-arm64' \
  '//:ctx_release_windows_x64' \
  '//:ctx_release_freebsd_x64' \
  '//:ctx_release_macos_arm64' \
  '//:ctx_release_macos_x64' \
  '.cdx.json.sha256' \
  '.third-party-notices.txt.sha256' \
  '.size.json' \
  '.candidate.json'; do
  grep -Fq -- "${required}" "${pipeline}" || {
    printf 'Buildkite release graph missing Bazel/evidence contract: %s\n' \
      "${required}" >&2
    exit 1
  }
done

for required in \
  'scripts/release-sbom.py verify-bundle' \
  '.cdx.json' \
  '.third-party-notices.txt'; do
  grep -Fq -- "${required}" "${staging}" || {
    printf 'GitHub staging missing verified CLI evidence: %s\n' "${required}" >&2
    exit 1
  }
done

python3 - "${matrix}" <<'PY'
import json
from pathlib import Path
import sys

value = json.loads(Path(sys.argv[1]).read_bytes())
targets = value["targets"]
expected = {
    "linux-x64",
    "linux-arm64",
    "macos-arm64",
    "macos-x64",
    "windows-x64",
    "freebsd-x64",
}
assert {target["id"] for target in targets} == expected
for target in targets:
    target_id = target["id"]
    assert target["public_construction_authority"] == "bazel-release-route-v1"
    assert target["public_construction_label"] == (
        f"//:ctx_release_{target_id.replace('-', '_')}"
    )
windows = next(target for target in targets if target["id"] == "windows-x64")
assert windows["public_rust_target"] == "x86_64-pc-windows-gnu"
assert windows["helper_rust_target"] == "x86_64-pc-windows-msvc"
PY

printf 'native Bazel packaging contract tests: OK\n'
