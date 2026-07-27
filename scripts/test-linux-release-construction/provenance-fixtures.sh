# Provenance fixture construction and rejection mutations shared by the release scenarios.

write_synthetic_build_info() {
  local matrix="$1"
  local platform="$2"
  local binary="$3"
  local target rust_sysroot local_runtime_status local_runtime_authority
  local_runtime_status=passed
  local_runtime_authority=authoritative
  case "${platform}" in
    linux-x64)
      target=x86_64-unknown-linux-gnu
      ;;
    linux-aarch64)
      target=aarch64-unknown-linux-gnu
      ;;
    windows-x64)
      target=x86_64-pc-windows-gnu
      local_runtime_status=not_run
      local_runtime_authority=not_run
      ;;
    freebsd-x64)
      target=x86_64-unknown-freebsd
      ;;
    *)
      echo "unsupported synthetic build-info platform: ${platform}" >&2
      return 2
      ;;
  esac
  if [[ "${platform}" == linux-* ]]; then
    rust_sysroot="/opt/rustup/toolchains/1.97.1-${target}"
    set -- \
      --expected-builder-base sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982 \
      --actual-builder-base sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982 \
      --builder-image-id sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
      --builder-recipe-sha256 dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd \
      --runtime-image-id sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
      --inspector-image-id sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc \
      --linux-builder-image docker.io/library/ubuntu:22.04@sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982 \
      --linux-ubuntu-snapshot 20260701T000000Z \
      --linux-glibc-max 2.35 \
      --linux-rust-toolchain 1.97.1 \
      --linux-rust-commit 8bab26f4f68e0e26f0bb7960be334d5b520ea452 \
      --linux-rust-sysroot "${rust_sysroot}"
  else
    set --
  fi
  python3 scripts/write-public-cli-build-info.py \
    --output "${matrix}/${binary}.build-info.json" \
    --artifact "${matrix}/${binary}" \
    --cargo-lock "${tmp_dir}/Cargo.lock" \
    --platform "${platform}" \
    --target "${target}" \
    --source-commit 0123456789abcdef0123456789abcdef01234567 \
    --source-clean true \
    --rust-version "rustc 1.97.1 (8bab26f4f 2026-07-14)" \
    "$@" \
    --static-status passed \
    --local-runtime-status "${local_runtime_status}" \
    --local-runtime-authority "${local_runtime_authority}"
}

mismatched_runtime_matrix="${tmp_dir}/mismatched-runtime-matrix"
cp -R "${complete_runtime_matrix}" "${mismatched_runtime_matrix}"
for binary in ctx; do
  printf 'synthetic %s\n' "${binary}" > "${mismatched_runtime_matrix}/${binary}"
  sha256sum "${mismatched_runtime_matrix}/${binary}" | awk '{ print $1 }' \
    > "${mismatched_runtime_matrix}/${binary}.sha256"
done
linux_binary_sha="$(cat "${mismatched_runtime_matrix}/ctx.sha256")"
linux_runtime_sha="$(sha256sum \
  "${mismatched_runtime_matrix}/ctx-onnxruntime-linux-x64.tar.gz" | awk '{ print $1 }')"
printf '%s\n' "${linux_runtime_sha}" > \
  "${mismatched_runtime_matrix}/ctx-onnxruntime-linux-x64.tar.gz.sha256"
write_synthetic_build_info "${mismatched_runtime_matrix}" linux-x64 ctx
linux_build_info_sha="$(sha256sum \
  "${mismatched_runtime_matrix}/ctx.build-info.json" | awk '{ print $1 }')"
cat > "${mismatched_runtime_matrix}/ctx-linux-x64.native-runtime-proof.txt" <<EOF
runtime=onnxruntime
embedding_backend=cpu
platform=linux-x64
host_system=Linux
host_arch=x86_64
host_native_arch=x86_64
process_translated=0
native_arch_probe=uname
runtime_authority=authoritative
artifact_sha256=${linux_binary_sha}
build_info_sha256=${linux_build_info_sha}
runtime_archive_sha256=ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff
semantic_search=passed
EOF
if "${stage_release_assets}" \
  "${mismatched_runtime_matrix}" "${tmp_dir}/mismatched-release" \
  >"${tmp_dir}/mismatched-runtime.out" 2>"${tmp_dir}/mismatched-runtime.err"; then
  echo "release staging accepted proof for a different runtime sidecar" >&2
  exit 1
fi
grep -Fq \
  'runtime proof does not match the exact runtime sidecar:' \
  "${tmp_dir}/mismatched-runtime.err"

duplicate_proof_matrix="${tmp_dir}/duplicate-proof-matrix"
cp -R "${mismatched_runtime_matrix}" "${duplicate_proof_matrix}"
printf 'platform=linux-x64\n' >> \
  "${duplicate_proof_matrix}/ctx-linux-x64.native-runtime-proof.txt"
if "${stage_release_assets}" \
  "${duplicate_proof_matrix}" "${tmp_dir}/duplicate-proof-release" \
  >"${tmp_dir}/duplicate-proof.out" 2>"${tmp_dir}/duplicate-proof.err"; then
  echo "release staging accepted a proof with duplicate fields" >&2
  exit 1
fi
grep -Fq \
  'runtime proof contains duplicate field platform:' \
  "${tmp_dir}/duplicate-proof.err"

missing_windows_dependency_matrix="${tmp_dir}/missing-windows-dependency-matrix"
cp -R "${complete_runtime_matrix}" "${missing_windows_dependency_matrix}"
write_synthetic_runtime_proof() {
  local platform="$1"
  local binary="$2"
  local proof="$3"
  local host_system="$4"
  local host_arch="$5"
  local runtime_asset="$6"
  local native_arch_probe="$7"
  local binary_sha runtime_sha build_info_line

  printf 'synthetic %s\n' "${platform}" > "${missing_windows_dependency_matrix}/${binary}"
  binary_sha="$(sha256sum "${missing_windows_dependency_matrix}/${binary}" | awk '{ print $1 }')"
  printf '%s\n' "${binary_sha}" > "${missing_windows_dependency_matrix}/${binary}.sha256"
  runtime_sha="$(sha256sum \
    "${missing_windows_dependency_matrix}/${runtime_asset}" | awk '{ print $1 }')"
  printf '%s\n' "${runtime_sha}" > \
    "${missing_windows_dependency_matrix}/${runtime_asset}.sha256"
  build_info_line=""
  case "${platform}" in
    linux-*|windows-x64|freebsd-x64)
      write_synthetic_build_info \
        "${missing_windows_dependency_matrix}" "${platform}" "${binary}"
      build_info_line="build_info_sha256=$(sha256sum \
        "${missing_windows_dependency_matrix}/${binary}.build-info.json" \
        | awk '{ print $1 }')"
      ;;
  esac
  cat > "${missing_windows_dependency_matrix}/${proof}" <<EOF
runtime=onnxruntime
embedding_backend=cpu
platform=${platform}
host_system=${host_system}
host_arch=${host_arch}
host_native_arch=${host_arch}
process_translated=0
native_arch_probe=${native_arch_probe}
runtime_authority=authoritative
artifact_sha256=${binary_sha}
${build_info_line}
runtime_archive_sha256=${runtime_sha}
semantic_search=passed
EOF
}
write_synthetic_runtime_proof \
  linux-x64 ctx ctx-linux-x64.native-runtime-proof.txt \
  Linux x86_64 ctx-onnxruntime-linux-x64.tar.gz uname
write_synthetic_runtime_proof \
  linux-aarch64 ctx-linux-aarch64 ctx-linux-aarch64.native-runtime-proof.txt \
  Linux aarch64 ctx-onnxruntime-linux-aarch64.tar.gz uname
write_synthetic_runtime_proof \
  macos-arm64 ctx-macos-arm64 ctx-macos-arm64.native-runtime-proof.txt \
  Darwin arm64 ctx-onnxruntime-macos-arm64.tar.gz sysctl
write_synthetic_runtime_proof \
  macos-x64 ctx-macos-x64 ctx-macos-x64.native-runtime-proof.txt \
  Darwin x86_64 ctx-onnxruntime-macos-x64.tar.gz sysctl
write_synthetic_runtime_proof \
  windows-x64 ctx.exe ctx-windows-x64.native-runtime-proof.txt \
  Windows_NT AMD64 ctx-onnxruntime-windows-x64.zip iswow64process2
write_synthetic_runtime_proof \
  freebsd-x64 ctx-freebsd-x64 ctx-freebsd-x64.native-runtime-proof.txt \
  FreeBSD amd64 ctx-onnxruntime-freebsd-x64.tar.gz uname

expect_provenance_rejection() {
  local name="$1"
  local expected="$2"
  local base_matrix="$3"
  local matrix="${tmp_dir}/${name}-matrix"
  local output="${tmp_dir}/${name}-release"
  cp -R "${base_matrix}" "${matrix}"
  shift 3
  "$@" "${matrix}"
  if "${stage_release_assets}" \
    "${matrix}" "${output}" \
    >"${tmp_dir}/${name}.out" 2>"${tmp_dir}/${name}.err"; then
    printf 'release staging accepted hostile provenance: %s\n' "${name}" >&2
    exit 1
  fi
  grep -Fq "${expected}" "${tmp_dir}/${name}.err"
  test ! -e "${output}"
}

mutate_linux_build_info() {
  local mode="$1"
  local matrix="$2"
  python3 - "${matrix}/ctx.build-info.json" "${mode}" <<'PY'
import json
import sys

path, mode = sys.argv[1:]
with open(path, encoding="utf-8") as source:
    value = json.load(source)
if mode == "dirty":
    value["source"]["clean"] = False
elif mode == "non-authoritative":
    value["gates"]["local_runtime_authority"] = "non_authoritative"
elif mode == "builder":
    value["linux_build"]["builder_image"] = (
        "docker.io/library/ubuntu:22.04@sha256:" + "f" * 64
    )
elif mode == "rust-sysroot":
    value["linux_build"]["rust_sysroot"] = "/tmp/caller-selected-sysroot"
elif mode == "static-abi":
    value["gates"]["static_abi"] = "not_run"
elif mode == "artifact":
    value["artifact_sha256"] = "0" * 64
else:
    raise SystemExit(f"unknown mutation: {mode}")
with open(path, "w", encoding="utf-8") as output:
    json.dump(value, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
}

corrupt_build_info_proof_binding() {
  local matrix="$1"
  sed -i \
    's/^build_info_sha256=.*/build_info_sha256=ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff/' \
    "${matrix}/ctx-linux-x64.native-runtime-proof.txt"
}

mutate_platform_build_info() {
  local binary="$1"
  local mode="$2"
  local matrix="$3"
  python3 - "${matrix}/${binary}.build-info.json" "${mode}" <<'PY'
import json
import sys

path, mode = sys.argv[1:]
with open(path, encoding="utf-8") as source:
    value = json.load(source)
if mode == "dirty":
    value["source"]["clean"] = False
elif mode == "artifact":
    value["artifact_sha256"] = "0" * 64
elif mode == "matrix":
    value["linux_build"] = {}
else:
    raise SystemExit(f"unknown mutation: {mode}")
with open(path, "w", encoding="utf-8") as output:
    json.dump(value, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
}

corrupt_platform_build_info_proof() {
  local proof="$1"
  local matrix="$2"
  sed -i \
    's/^build_info_sha256=.*/build_info_sha256=ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff/' \
    "${matrix}/${proof}"
}
