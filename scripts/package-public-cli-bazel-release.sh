#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: bazel run --config=release //:ctx_release_<target> -- [--output-dir PATH] [--build-info PATH]

Packages the exact target-configured //crates/ctx-cli:ctx Bazel output declared
by the selected release route. The tool never builds, publishes, or invokes
Cargo. Linux requires build-info from the pinned builder because only that
builder can author its provenance.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

usage_error() {
  printf 'error: %s\n' "$*" >&2
  usage
  exit 64
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v sha256 >/dev/null 2>&1; then
    sha256 -q "$1"
  else
    die "sha256sum, shasum, or sha256 is required"
  fi
}

resolve_declared_runfile() {
  python3 - "$1" <<'PY'
import os
import posixpath
import stat
import sys
from pathlib import Path

key = sys.argv[1]
if (
    not key
    or key.startswith("/")
    or posixpath.normpath(key) != key
    or key == ".."
    or key.startswith("../")
):
    raise SystemExit(1)

workspace = os.environ.get("TEST_WORKSPACE", "_main")
candidates = []
runfiles_dir = os.environ.get("RUNFILES_DIR")
if runfiles_dir:
    candidates.extend(
        [
            Path(runfiles_dir) / workspace / key,
            Path(runfiles_dir) / "_main" / key,
        ]
    )

manifest = os.environ.get("RUNFILES_MANIFEST_FILE")
if manifest:
    logical = {f"{workspace}/{key}", f"_main/{key}"}
    try:
        with open(manifest, encoding="utf-8") as source:
            for line in source:
                name, separator, value = line.rstrip("\n").partition(" ")
                if separator and name in logical:
                    candidates.append(Path(value))
    except OSError:
        pass

for candidate in candidates:
    try:
        resolved = candidate.resolve(strict=True)
        mode = resolved.stat().st_mode
    except OSError:
        continue
    if stat.S_ISREG(mode):
        print(os.path.realpath(resolved))
        break
else:
    raise SystemExit(1)
PY
}

regular_user_file() {
  python3 - "$1" <<'PY'
import os
import stat
import sys
from pathlib import Path

path = Path(sys.argv[1])
if not path.is_absolute():
    path = Path.cwd() / path
try:
    mode = path.lstat().st_mode
except OSError:
    raise SystemExit(1)
if stat.S_ISLNK(mode) or not stat.S_ISREG(mode):
    raise SystemExit(1)
print(os.path.realpath(path))
PY
}

script_repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
if [[ -n "${BUILD_WORKSPACE_DIRECTORY:-}" \
  && -f "${BUILD_WORKSPACE_DIRECTORY}/scripts/public-cli-release-targets.py" ]]; then
  repo_root="$(cd "${BUILD_WORKSPACE_DIRECTORY}" && pwd -P)"
else
  repo_root="${script_repo_root}"
fi

artifact_runfile=""
rustc_runfile=""
sbom_inventory_runfile=""
cargo_lock_runfile=""
target_matrix_runfile=""
target_id=""
output_dir="target/public-cli-artifacts"
build_info=""
seen_artifact=0
seen_rustc=0
seen_sbom_inventory=0
seen_cargo_lock=0
seen_target_matrix=0
seen_target=0
seen_output=0
seen_build_info=0

while [[ $# -gt 0 ]]; do
  option="$1"
  case "${option}" in
    --declared-artifact-runfile)
      seen_artifact=$((seen_artifact + 1))
      [[ "${seen_artifact}" == "1" ]] \
        || usage_error "duplicate reserved argument: ${option}"
      shift
      [[ $# -gt 0 && -n "$1" ]] || usage_error "${option} requires a value"
      artifact_runfile="$1"
      ;;
    --declared-rustc-runfile)
      seen_rustc=$((seen_rustc + 1))
      [[ "${seen_rustc}" == "1" ]] \
        || usage_error "duplicate reserved argument: ${option}"
      shift
      [[ $# -gt 0 && -n "$1" ]] || usage_error "${option} requires a value"
      rustc_runfile="$1"
      ;;
    --declared-sbom-inventory-runfile)
      seen_sbom_inventory=$((seen_sbom_inventory + 1))
      [[ "${seen_sbom_inventory}" == "1" ]] \
        || usage_error "duplicate reserved argument: ${option}"
      shift
      [[ $# -gt 0 && -n "$1" ]] || usage_error "${option} requires a value"
      sbom_inventory_runfile="$1"
      ;;
    --declared-cargo-lock-runfile)
      seen_cargo_lock=$((seen_cargo_lock + 1))
      [[ "${seen_cargo_lock}" == "1" ]] \
        || usage_error "duplicate reserved argument: ${option}"
      shift
      [[ $# -gt 0 && -n "$1" ]] || usage_error "${option} requires a value"
      cargo_lock_runfile="$1"
      ;;
    --declared-target-matrix-runfile)
      seen_target_matrix=$((seen_target_matrix + 1))
      [[ "${seen_target_matrix}" == "1" ]] \
        || usage_error "duplicate reserved argument: ${option}"
      shift
      [[ $# -gt 0 && -n "$1" ]] || usage_error "${option} requires a value"
      target_matrix_runfile="$1"
      ;;
    --declared-target)
      seen_target=$((seen_target + 1))
      [[ "${seen_target}" == "1" ]] \
        || usage_error "duplicate reserved argument: ${option}"
      shift
      [[ $# -gt 0 && -n "$1" ]] || usage_error "${option} requires a value"
      target_id="$1"
      ;;
    --output-dir)
      seen_output=$((seen_output + 1))
      [[ "${seen_output}" == "1" ]] || usage_error "duplicate argument: ${option}"
      shift
      [[ $# -gt 0 && -n "$1" ]] || usage_error "${option} requires a value"
      output_dir="$1"
      ;;
    --build-info)
      seen_build_info=$((seen_build_info + 1))
      [[ "${seen_build_info}" == "1" ]] || usage_error "duplicate argument: ${option}"
      shift
      [[ $# -gt 0 && -n "$1" ]] || usage_error "${option} requires a value"
      build_info="$1"
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --artifact|--rustc|--target|--source-commit|--target-matrix)
      usage_error "${option} is route-owned and cannot be supplied by the caller"
      ;;
    *)
      usage_error "unknown argument: ${option}"
      ;;
  esac
  shift
done

[[ "${seen_artifact}:${seen_rustc}:${seen_sbom_inventory}:${seen_cargo_lock}:${seen_target_matrix}:${seen_target}" \
  == "1:1:1:1:1:1" ]] || usage_error "release route declarations are incomplete"

artifact="$(resolve_declared_runfile "${artifact_runfile}")" \
  || die "declared Bazel artifact runfile is unavailable"
rustc="$(resolve_declared_runfile "${rustc_runfile}")" \
  || die "declared Bazel rustc runfile is unavailable"
sbom_inventory="$(resolve_declared_runfile "${sbom_inventory_runfile}")" \
  || die "declared target SBOM inventory runfile is unavailable"
declared_cargo_lock="$(resolve_declared_runfile "${cargo_lock_runfile}")" \
  || die "declared Cargo.lock runfile is unavailable"
target_matrix="$(resolve_declared_runfile "${target_matrix_runfile}")" \
  || die "declared release-target matrix runfile is unavailable"

target_values="$(python3 -B "${repo_root}/scripts/public-cli-release-targets.py" \
  --matrix "${target_matrix}" shell "${target_id}")" || exit $?
eval "${target_values}"

[[ -x "${artifact}" ]] || die "declared Bazel artifact is not executable: ${artifact}"
[[ -x "${rustc}" ]] || die "declared Bazel rustc is not executable: ${rustc}"
bazel_binary="ctx"
[[ "${CTX_PUBLIC_TARGET_OS}" == "windows" ]] && bazel_binary="ctx.exe"
[[ "$(basename "${artifact}")" == "${bazel_binary}" ]] \
  || die "declared Bazel artifact name does not match target graph"

rustc_version="$("${rustc}" --version)"
[[ "${rustc_version}" =~ ^rustc\ 1\.97\.1\ \(8bab26f4f ]] \
  || die "declared Bazel rustc is not the pinned 1.97.1 toolchain"

source_repo="${BUILD_WORKSPACE_DIRECTORY:-${repo_root}}"
source_repo="$(cd "${source_repo}" && pwd -P)"
git -C "${source_repo}" rev-parse --is-inside-work-tree >/dev/null 2>&1 \
  || die "release packaging requires a Git source workspace"
source_commit="$(git -C "${source_repo}" rev-parse --verify HEAD^{commit})"
[[ "${source_commit}" =~ ^[0-9a-f]{40}$ && ! "${source_commit}" =~ ^0{40}$ ]] \
  || die "source commit is not a nonzero lowercase 40-hex commit"
[[ -z "$(git -C "${source_repo}" status --porcelain=v1 --untracked-files=all)" ]] \
  || die "release packaging requires a clean source checkout"

workspace_cargo_lock="${source_repo}/Cargo.lock"
[[ -f "${workspace_cargo_lock}" && ! -L "${workspace_cargo_lock}" ]] \
  || die "packaging checkout Cargo.lock is not a regular file"
cargo_lock_sha256="$(sha256_file "${declared_cargo_lock}")"
[[ "${cargo_lock_sha256}" == "$(sha256_file "${workspace_cargo_lock}")" ]] \
  || die "declared Cargo.lock does not match the packaging checkout"

identity_env=(env -i "PATH=${PATH:-/usr/bin:/bin}")
for environment_name in SystemRoot SYSTEMROOT WINDIR; do
  if [[ -n "${!environment_name:-}" ]]; then
    identity_env+=("${environment_name}=${!environment_name}")
  fi
done
identity_output="$("${identity_env[@]}" "${artifact}" _release-build-identity)" \
  || die "declared Bazel artifact does not expose a release build identity"
expected_identity="$(
  printf 'CTX_RELEASE_BUILD_SOURCE_COMMIT=%s\n' "${source_commit}"
  printf 'CTX_RELEASE_BUILD_CARGO_LOCK_SHA256=%s\n' "${cargo_lock_sha256}"
  printf 'CTX_RELEASE_BUILD_TARGET=%s' "${CTX_PUBLIC_TARGET_TRIPLE}"
)"
[[ "${identity_output}" == "${expected_identity}" ]] \
  || die "declared Bazel artifact identity does not match source, Cargo.lock, and target graph"
artifact_sha_before="$(sha256_file "${artifact}")"

version="$(
  python3 -B "${repo_root}/scripts/release/public-cli-bazel-build-info.py" \
    cargo-version \
    --cargo-toml "${repo_root}/crates/ctx-cli/Cargo.toml"
)" || die "could not determine the ctx package version"

if [[ "${output_dir}" != /* ]]; then
  output_dir="${source_repo}/${output_dir}"
fi
output_dir="$(python3 - "${output_dir}" <<'PY'
import os
import sys
print(os.path.abspath(sys.argv[1]))
PY
)"
case "${output_dir}/" in
  "${source_repo}/"*)
    git -C "${source_repo}" check-ignore -q -- "${output_dir}/.ctx-release-output" \
      || die "release output inside the checkout must be ignored by Git: ${output_dir}"
    ;;
esac

binary_name="${CTX_PUBLIC_TARGET_BINARY}"
reserved_leaves=(
  "${binary_name}"
  "${binary_name}.build-info.json"
  "${binary_name}.cdx.json"
  "${binary_name}.expected-version"
  "${binary_name}.sha256"
  "${binary_name}.version"
)
if [[ "${CTX_PUBLIC_TARGET_OS}" == "macos" ]]; then
  for suffix in \
    attestation.cms attestation.json codesign.txt execution.txt notary-log.json \
    notary-log.stderr notary-submit.json notary-submit.stderr signing.json; do
    reserved_leaves+=("ctx-${CTX_PUBLIC_TARGET_PLATFORM}.${suffix}")
  done
fi

preflight_args=(
  --stage "${output_dir}"
  --output "${output_dir}"
  --file "${binary_name}"
  --check-only
)
for name in "${reserved_leaves[@]}"; do
  preflight_args+=(--reserve "${name}")
done
python3 -B "${repo_root}/scripts/install-public-cli-candidate.py" \
  "${preflight_args[@]}"

stage_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-bazel-release.XXXXXX")"
cleanup() {
  if [[ -n "${stage_dir:-}" \
    && "${stage_dir}" == "${TMPDIR:-/tmp}/ctx-bazel-release."* \
    && -d "${stage_dir}" ]]; then
    rm -rf -- "${stage_dir}"
  fi
}
trap cleanup EXIT

staged="${stage_dir}/${binary_name}"
install -m 0755 "${artifact}" "${staged}"
[[ "$(sha256_file "${staged}")" == "${artifact_sha_before}" ]] \
  || die "staged artifact does not match the declared Bazel output"

macos_signing_mode="${CTX_MACOS_RELEASE_SIGNING:-optional}"
if [[ "${CTX_PUBLIC_CLI_ARTIFACT_MATRIX:-0}" == "1" \
  && "${CTX_PUBLIC_TARGET_OS}" == "macos" ]]; then
  macos_signing_mode=required
fi
case "${macos_signing_mode}" in
  optional|required) ;;
  *) die "CTX_MACOS_RELEASE_SIGNING must be optional or required" ;;
esac
if [[ "${CTX_PUBLIC_TARGET_OS}" == "macos" \
  && "${macos_signing_mode}" == "required" ]]; then
  macos_signing_authority="${CTX_MACOS_SIGNING_AUTHORITY:-}"
  case "${macos_signing_authority}" in
    staging|dev) ;;
    *) die "required macOS signing needs CTX_MACOS_SIGNING_AUTHORITY=staging or dev" ;;
  esac
  "${repo_root}/scripts/run-macos-release-signing.sh" \
    --authority "${macos_signing_authority}" \
    "${CTX_PUBLIC_TARGET_PLATFORM}" cli "${staged}" "${stage_dir}"
  "${repo_root}/scripts/verify-macos-signed-cli.sh" \
    "${CTX_PUBLIC_TARGET_PLATFORM}" "${staged}" "${version}" \
    "${stage_dir}/ctx-${CTX_PUBLIC_TARGET_PLATFORM}.signing.json"
fi

staged_identity_output="$("${identity_env[@]}" "${staged}" _release-build-identity)" \
  || die "staged artifact lost its release build identity"
[[ "${staged_identity_output}" == "${identity_output}" ]] \
  || die "staged artifact release identity differs from the declared Bazel output"

packaged_sha="$(sha256_file "${staged}")"
printf '%s\n' "${packaged_sha}" >"${staged}.sha256"

host_os="$(uname -s 2>/dev/null || true)"
host_arch="$(uname -m 2>/dev/null || true)"
can_run_on_host=0
case "${target_id}:${host_os}:${host_arch}" in
  linux-x64:Linux:x86_64|\
  linux-arm64:Linux:aarch64|\
  linux-arm64:Linux:arm64|\
  macos-arm64:Darwin:arm64|\
  macos-x64:Darwin:x86_64|\
  freebsd-x64:FreeBSD:amd64|\
  windows-x64:MINGW*:x86_64|\
  windows-x64:MSYS*:x86_64|\
  windows-x64:CYGWIN*:x86_64)
    can_run_on_host=1
    ;;
esac
if [[ "${target_id}" == "macos-x64" && "${host_os}" == "Darwin" \
  && -x /usr/bin/arch ]] \
  && /usr/bin/arch -x86_64 /usr/bin/true >/dev/null 2>&1; then
  can_run_on_host=1
fi
[[ "${can_run_on_host}" == "1" ]] \
  || die "target ${target_id} packaging requires its native runtime authority"

if [[ "${target_id}" == "macos-x64" && "${host_arch}" != "x86_64" ]]; then
  version_output="$(/usr/bin/arch -x86_64 "${staged}" --version)"
else
  version_output="$("${staged}" --version)"
fi
[[ "${version_output}" == "ctx ${version}" ]] \
  || die "candidate version mismatch: expected ctx ${version}, got ${version_output}"
printf '%s\n' "${version_output}" >"${staged}.version"

smoke_result="${stage_dir}/.candidate-smoke.json"
if [[ "${CTX_PUBLIC_TARGET_OS}" == "windows" ]]; then
  powershell_bin="$(command -v powershell.exe 2>/dev/null || command -v pwsh 2>/dev/null || true)"
  [[ -n "${powershell_bin}" ]] || die "native Windows candidate smoke requires PowerShell"
  "${powershell_bin}" -NoLogo -NoProfile -NonInteractive \
    -ExecutionPolicy Bypass \
    -File "${repo_root}/scripts/run-native-candidate-smoke.ps1" \
    -Binary "${staged}" \
    -Fixture "${repo_root}/tests/fixtures/custom-history-jsonl/basic.jsonl" \
    -ExpectedVersion "${version}" \
    -ResultPath "${smoke_result}"
else
  "${repo_root}/scripts/run-native-candidate-smoke.sh" \
    "${staged}" \
    "${repo_root}/tests/fixtures/custom-history-jsonl/basic.jsonl" \
    "${version}" \
    "${smoke_result}"
fi
grep -Fq '"status":"passed"' "${smoke_result}" \
  || die "native candidate smoke did not record a pass"

CTX_PUBLIC_CLI_EXPECTED_VERSION="${version}" \
  "${repo_root}/scripts/check-public-cli-artifact.sh" \
  "${CTX_PUBLIC_TARGET_PLATFORM}" "${stage_dir}"

if [[ "${CTX_PUBLIC_TARGET_OS}" == "macos" \
  && "${macos_signing_mode}" == "required" ]]; then
  "${repo_root}/scripts/check-macos-release-signing.sh" \
    "${CTX_PUBLIC_TARGET_PLATFORM}" cli "${staged}" \
    "${stage_dir}/ctx-${CTX_PUBLIC_TARGET_PLATFORM}.signing.json"
fi

IFS=$'\t' read -r \
  host_system observed_arch host_native_arch process_translated _native_arch_probe \
  hardware_identity emulation hypervisor evidence_complete \
  < <("${repo_root}/scripts/public-cli-host-runtime-evidence.sh")
local_runtime_authority="$("${repo_root}/scripts/public-cli-runtime-authority.sh" \
  "${CTX_PUBLIC_TARGET_PLATFORM}" "${host_system}" "${observed_arch}" \
  passed "${host_native_arch}" "${process_translated}" \
  "${hardware_identity}" "${emulation}" "${hypervisor}" "${evidence_complete}" \
  "${CTX_RELEASE_MACOS_X64_KVM_RUNNER_ID:-}")"

staged_build_info="${staged}.build-info.json"
if [[ "${CTX_PUBLIC_TARGET_OS}" == "linux" ]]; then
  [[ -n "${build_info}" ]] \
    || die "Linux Bazel packaging requires builder-authored --build-info"
  build_info="$(regular_user_file "${build_info}")" \
    || die "builder-authored build-info must be a regular non-symlink file"
  install -m 0644 "${build_info}" "${staged_build_info}"
else
  python3 -B "${repo_root}/scripts/write-public-cli-build-info.py" \
    --output "${staged_build_info}" \
    --artifact "${staged}" \
    --cargo-lock "${workspace_cargo_lock}" \
    --platform "${CTX_PUBLIC_TARGET_PLATFORM}" \
    --target "${CTX_PUBLIC_TARGET_TRIPLE}" \
    --source-commit "${source_commit}" \
    --source-clean true \
    --rust-version "${rustc_version}" \
    --static-status passed \
    --local-runtime-status passed \
    --local-runtime-authority "${local_runtime_authority}"
fi

python3 -I "${repo_root}/scripts/check-public-cli-build-info.py" \
  --artifact "${staged}" \
  --build-info "${staged_build_info}" \
  --matrix "${target_matrix}" \
  --platform "${CTX_PUBLIC_TARGET_PLATFORM}" \
  --source-commit "${source_commit}" \
  --cargo-lock "${workspace_cargo_lock}" >/dev/null
if [[ "${CTX_PUBLIC_TARGET_OS}" == "linux" ]]; then
  python3 -I "${repo_root}/scripts/release/public-cli-bazel-build-info.py" verify \
    --artifact "${staged}" \
    --bazel-version-file "${repo_root}/.bazelversion" \
    --build-info "${staged_build_info}" \
    --builder-recipe \
      "${repo_root}/scripts/release/linux-bazel-release.Dockerfile" \
    --cargo-lock "${workspace_cargo_lock}" \
    --cargo-toml "${repo_root}/crates/ctx-cli/Cargo.toml" \
    --matrix "${target_matrix}" \
    --module-file "${repo_root}/MODULE.bazel" \
    --module-lock "${repo_root}/MODULE.bazel.lock" \
    --platform "${CTX_PUBLIC_TARGET_PLATFORM}" \
    --rustc "${rustc}" \
    --source-commit "${source_commit}" \
    --source-repo "${source_repo}" \
    --version "${version}" >/dev/null
fi

staged_sbom="${staged}.cdx.json"
python3 -I "${repo_root}/scripts/release-sbom.py" generate \
  --product core \
  --version "${version}" \
  --platform "${CTX_PUBLIC_TARGET_PLATFORM}" \
  --artifact "${staged}" \
  --build-info "${staged_build_info}" \
  --cargo-lock "${workspace_cargo_lock}" \
  --module-lock "${repo_root}/MODULE.bazel.lock" \
  --module-file "${repo_root}/MODULE.bazel" \
  --target-inventory "${sbom_inventory}" \
  --output "${staged_sbom}" >/dev/null

if [[ "${CTX_PUBLIC_TARGET_OS}" == "windows" ]]; then
  printf '%s\n' "${version}" >"${staged}.expected-version"
fi

[[ "$(git -C "${source_repo}" rev-parse --verify HEAD^{commit})" == "${source_commit}" ]] \
  || die "source commit changed during release packaging"
[[ -z "$(git -C "${source_repo}" status --porcelain=v1 --untracked-files=all)" ]] \
  || die "source checkout changed during release packaging"
[[ "$(sha256_file "${declared_cargo_lock}")" == "${cargo_lock_sha256}" \
  && "$(sha256_file "${workspace_cargo_lock}")" == "${cargo_lock_sha256}" ]] \
  || die "Cargo.lock changed during release packaging"
[[ "$(sha256_file "${artifact}")" == "${artifact_sha_before}" ]] \
  || die "declared Bazel output changed during release packaging"
[[ "$(sha256_file "${staged}")" == "${packaged_sha}" ]] \
  || die "staged artifact changed after release checks"
[[ "$("${identity_env[@]}" "${staged}" _release-build-identity)" == "${identity_output}" ]] \
  || die "staged artifact identity changed after release checks"
python3 -I "${repo_root}/scripts/release-sbom.py" verify \
  --product core \
  --version "${version}" \
  --platform "${CTX_PUBLIC_TARGET_PLATFORM}" \
  --artifact "${staged}" \
  --build-info "${staged_build_info}" \
  --cargo-lock "${workspace_cargo_lock}" \
  --module-lock "${repo_root}/MODULE.bazel.lock" \
  --module-file "${repo_root}/MODULE.bazel" \
  --target-inventory "${sbom_inventory}" \
  --sbom "${staged_sbom}" >/dev/null

install_args=(
  --stage "${stage_dir}"
  --output "${output_dir}"
  --sha256 "${binary_name}=${packaged_sha}"
)
for name in "${reserved_leaves[@]}"; do
  install_args+=(--reserve "${name}")
done
for path in "${stage_dir}"/*; do
  [[ -e "${path}" ]] || continue
  [[ -f "${path}" && ! -L "${path}" ]] \
    || die "staged candidate output is not a regular file: ${path}"
  install_args+=(--file "$(basename "${path}")")
done
python3 -B "${repo_root}/scripts/install-public-cli-candidate.py" \
  "${install_args[@]}"

trap - EXIT
cleanup
printf 'public CLI Bazel candidate: %s\n' "${output_dir}/${binary_name}"
printf 'public CLI distribution artifact: %s\n' "${CTX_PUBLIC_TARGET_ARTIFACT}"
printf 'public CLI source commit: %s\n' "${source_commit}"
printf 'public CLI sha256: %s\n' "${packaged_sha}"
