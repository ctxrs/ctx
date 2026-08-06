#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  source_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  source_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fi

wrapper="${source_root}/scripts/release/build-linux-bazel-release.sh"
controller="${source_root}/scripts/release/run-linux-bazel-release-controller.sh"
controller_recipe="${source_root}/scripts/release/linux-bazel-release-controller.Dockerfile"
controller_receipt="${source_root}/scripts/release/write-linux-bazel-controller-receipt.py"
bundle="${source_root}/scripts/release/release_bundle.py"
recipe="${source_root}/scripts/release/linux-bazel-release.Dockerfile"
pipeline="${source_root}/.buildkite/pipeline.yml"
matrix="${source_root}/contracts/release-targets-v1.json"
staging="${source_root}/scripts/stage-github-release-assets.sh"
semantic_staging="${source_root}/scripts/stage-semantic-release-handoff.sh"
packager="${source_root}/scripts/package-public-cli-bazel-release.sh"
release_routes="${source_root}/tools/bazel/release_routes.bzl"
release_config="${source_root}/.bazelrc"
module_definition="${source_root}/MODULE.bazel"
module_lock="${source_root}/MODULE.bazel.lock"
rules_rust_patch="${source_root}/tools/bazel/patches/rules-rust-freebsd-host.patch"

grep -Fxq 'build:release --lockfile_mode=error' "${release_config}" || {
  echo 'release config must fail closed on incomplete module lock data' >&2
  exit 1
}

python3 - "${module_lock}" <<'PY'
import json
from pathlib import Path
import sys

lock = json.loads(Path(sys.argv[1]).read_bytes())
extension = lock["moduleExtensions"]["@@rules_go~//go:extensions.bzl%go_sdk"]
expected = {
    "os:linux,arch:amd64",
    "os:linux,arch:aarch64",
    "os:freebsd,arch:amd64",
    "os:osx,arch:x86_64",
    "os:osx,arch:aarch64",
    "os:windows,arch:amd64",
}
if set(extension) != expected:
    raise SystemExit(
        "rules_go host lock factors differ: "
        f"expected {sorted(expected)}, observed {sorted(extension)}"
    )
for digest_field in ("bzlTransitiveDigest", "usagesDigest"):
    digests = {entry[digest_field] for entry in extension.values()}
    if len(digests) != 1:
        raise SystemExit(
            f"host factors disagree on {digest_field}: {sorted(digests)}"
        )
for factor, entry in extension.items():
    if not entry["generatedRepoSpecs"]:
        raise SystemExit(f"{factor} has no generated repository specs")
PY

grep -Fq 'patches = ["//tools/bazel/patches:rules-rust-freebsd-host.patch"]' \
  "${module_definition}" || {
  echo 'rules_rust must retain the pinned FreeBSD host patch' >&2
  exit 1
}
for required_patch_line in \
  '+        "freebsd": ["x86_64"],' \
  '+    if "freebsd" in repository_ctx.os.name:' \
  '+        return triple("{}-unknown-freebsd".format(arch))'; do
  grep -Fq -- "${required_patch_line}" "${rules_rust_patch}" || {
    echo "rules_rust FreeBSD host patch is missing: ${required_patch_line}" >&2
    exit 1
  }
done

for required in \
  'route_target=//:ctx_release_linux_x64' \
  'route_target=//:ctx_release_linux_arm64' \
  'docker_platform=linux/amd64' \
  'docker_platform=linux/arm64' \
  'requires a native ${expected_host_arch} host' \
  'requires a native ${expected_host_arch} Docker daemon' \
  'use the pinned Ubuntu 22 controller route' \
  'scripts/public-cli-host-runtime-evidence.sh' \
  'scripts/public-cli-runtime-authority.sh' \
  '/build/release-input/${CTX_RELEASE_BINARY_NAME}.build-info.json' \
  'scripts/release/detached-debug-symbols.py prepare' \
  '/build/release-symbol-output/bundle' \
  'scripts/release/release_bundle.py seal' \
  'scripts/release/release_bundle.py verify' \
  'scripts/release/release_bundle.py "${commit_args[@]}"' \
  '--seal-sha256 "${seal_sha256}"' \
  'scripts/run-native-candidate-smoke.sh' \
  'scripts/smoke-daemon-semantic-release.sh' \
  'scripts/build-onnxruntime-sidecar.sh' \
  '"${task_root}/cache"' \
  '--transcode-runtime "${CTX_PUBLIC_TARGET_PLATFORM}"' \
  'ctx-${CTX_PUBLIC_TARGET_PLATFORM}.release-complete.json' \
  '--network none' \
  '--lockfile_mode=error' \
  '${CTX_PUBLIC_TARGET_BINARY}.cdx.json.sha256' \
  '${CTX_PUBLIC_TARGET_BINARY}.third-party-notices.txt.sha256' \
  '${CTX_PUBLIC_TARGET_BINARY}.size.json' \
  '${CTX_PUBLIC_TARGET_BINARY}.candidate.json'; do
  grep -Fq -- "${required}" "${wrapper}" || {
    printf 'native Linux Bazel wrapper missing contract: %s\n' "${required}" >&2
    exit 1
  }
done

for required in \
  'docker.io/library/ubuntu:22.04@sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982' \
  'scripts/public-cli-runtime-authority.sh' \
  'pinned Ubuntu 22 outer controller is not authoritative' \
  'CTX_ONNXRUNTIME_CACHE_DIR=${sidecar_cache}' \
  'd304445daa7e6429293dc02035063b7993fb6a489ee90d8851bff497952836dc' \
  '50eed4c67aef71f5a33e82df66788f5415840c66827b6ef2fdf799a046ad59de' \
  'scripts/release/build-linux-bazel-release.sh' \
  'scripts/release/write-linux-bazel-controller-receipt.py' \
  '--controller-receipt'; do
  grep -Fq -- "${required}" "${controller}" || {
    printf 'Linux controller wrapper missing contract: %s\n' "${required}" >&2
    exit 1
  }
done
for required in \
  'org.ctx.release.role="ctx-public-bazel-controller"' \
  'DOCKER_VERSION="27.5.1"' \
  'BUILDX_VERSION="0.20.1"' \
  'zstd' \
  'getconf GNU_LIBC_VERSION' \
  'glibc 2.35'; do
  grep -Fq -- "${required}" "${controller_recipe}" || {
    printf 'Linux controller recipe missing contract: %s\n' "${required}" >&2
    exit 1
  }
done
grep -Fq 'outer controller is not authoritative' "${controller_receipt}"
if grep -Fq -- '--construction-host' \
  "${source_root}/scripts/public-cli-runtime-authority.sh" \
  "${wrapper}" "${controller}"; then
  echo 'Linux release route retains a second relaxed authority scope' >&2
  exit 1
fi

if grep -Fq -- '-v "${output_dir}:' "${wrapper}" \
  || grep -Fq -- 'mktemp -d "${private_symbols_parent}/' "${wrapper}" \
  || grep -Eq -- 'mv .*private_symbols_dir' "${wrapper}"; then
  echo "native Linux builder touches final destinations before publication" >&2
  exit 1
fi
for forbidden in \
  'publish-linux-bazel-release.py' \
  'completed_candidate_io.py' \
  '/proc/self/fd' \
  'cargo build' \
  'cargo zigbuild' \
  'qemu-'; do
  if grep -Fq -- "${forbidden}" \
    "${wrapper}" "${controller}" "${controller_receipt}"; then
    printf 'native Linux route retains retired construction: %s\n' \
      "${forbidden}" >&2
    exit 1
  fi
done

for required in \
  'RENAME_NOREPLACE' \
  'ctx-public-linux-release-completion' \
  'seal_sha256' \
  'os.fsync' \
  'shlex.quote' \
  'release stage must be a sibling of its final destination' \
  'release destination already exists'; do
  grep -Fq -- "${required}" "${bundle}" || {
    printf 'release bundle utility missing contract: %s\n' "${required}" >&2
    exit 1
  }
done
for forbidden in \
  '/proc/self/fd' \
  'DescriptorBinding' \
  'PinnedTreeSnapshot' \
  'pass_fds' \
  'subprocess'; do
  if grep -Fq -- "${forbidden}" "${bundle}"; then
    printf 'release bundle utility retains retired mechanism: %s\n' \
      "${forbidden}" >&2
    exit 1
  fi
done

for required in \
  'detached-debug-symbols.py" prepare' \
  'detached-debug-symbols.py" finalize' \
  'public CLI private debug symbols:' \
  '_release-build-identity' \
  '"${sbom_tool}" generate' \
  '"${sbom_tool}" verify' \
  '"${sbom_tool}" verify-bundle'; do
  grep -Fq -- "${required}" "${packager}" || {
    printf 'native release packager missing contract: %s\n' "${required}" >&2
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

[[ "$(tr -d '[:space:]' <"${source_root}/.bazelversion")" == "7.7.1" ]] || {
  echo 'native release checksum contract expects Bazel 7.7.1' >&2
  exit 1
}
for required in \
  'bazel_binary_sha256=115a1b62be95f29e5821d4dddffba1b058905a48019b499919c285e7f708d5e2' \
  'bazel_binary_sha256=71df04ec724f1b577f1f47ec9a6b81d13f39683f6c3215cacf45fdaf40b2c5c1'; do
  grep -Fq -- "${required}" "${wrapper}" || {
    printf 'native Linux release wrapper has a stale Bazel checksum: %s\n' \
      "${required}" >&2
    exit 1
  }
done

for required in \
  'release_bundle.py' \
  'verify-bundle' \
  '.build-info.json' \
  '.cdx.json' \
  '.third-party-notices.txt' \
  '--platform linux-x64' \
  '--platform linux-aarch64' \
  'commit-directory' \
  'HEAD^{commit}'; do
  grep -Fq -- "${required}" "${staging}" || {
    printf 'GitHub staging missing release contract: %s\n' "${required}" >&2
    exit 1
  }
done

for required in \
  'release_bundle.py' \
  'construct-semantic-release-catalog.sh' \
  'SHA256SUMS' \
  'commit-directory' \
  'ctx-multilingual-e5-small-onnx-fp32-1.0.0.tar.xz' \
  'ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0.tar.xz' \
  'ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz' \
  'ctx-onnxruntime-linux-x64.tar.zst' \
  'ctx-onnxruntime-linux-aarch64.tar.zst' \
  'ctx-onnxruntime-macos-arm64.tar.zst' \
  'ctx-onnxruntime-macos-x64.tar.zst' \
  'ctx-windowsml-windows-x64.zip' \
  'ctx-onnxruntime-freebsd-x64.tar.zst' \
  'ctx-onnxruntime-linux-x64-cuda12.tar.zst'; do
  grep -Fq -- "${required}" "${semantic_staging}" || {
    printf 'Semantic staging missing exact asset contract: %s\n' \
      "${required}" >&2
    exit 1
  }
done
for forbidden in \
  'release-complete.json' \
  'HEAD^{commit}' \
  'source_commit' \
  'publish-linux-bazel-release.py' \
  '/proc/self/fd'; do
  if grep -Fq -- "${forbidden}" "${semantic_staging}"; then
    printf 'Semantic staging retains irrelevant Linux protocol: %s\n' \
      "${forbidden}" >&2
    exit 1
  fi
done

if grep -Fq 'build-public-cli-artifact.sh' "${pipeline}"; then
  echo "Buildkite release candidates still use the Cargo constructor" >&2
  exit 1
fi
for required in \
  'scripts/release/build-linux-bazel-release.sh' \
  '--native-smoke-dir target/public-cli-native-smoke/linux-x64' \
  '--native-smoke-dir target/public-cli-native-smoke/linux-aarch64' \
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

python3 - "${matrix}" <<'PY'
import json
from pathlib import Path
import sys

targets = json.loads(Path(sys.argv[1]).read_bytes())["targets"]
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
PY

printf 'native Bazel packaging contract tests: OK\n'
