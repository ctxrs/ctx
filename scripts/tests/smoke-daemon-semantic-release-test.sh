#!/usr/bin/env bash
set -euo pipefail
umask 0002

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  repo_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fi
tmp="$(mktemp -d "${TMPDIR:-/tmp}/ctx-semantic-release-smoke-test.XXXXXX")"
trap 'rm -rf "${tmp}"' EXIT
export CTX_PUBLIC_CLI_RUNTIME_AUTHORITY_BASELINE=ubuntu-24.04

release_root="${tmp}/release-root"
mkdir -p "${release_root}/contracts" "${release_root}/scripts/release"
for release_script in \
  check-public-cli-build-info.py \
  dev-install-from-metadata.sh \
  public-cli-host-runtime-evidence.sh \
  public-cli-runtime-authority.sh \
  semantic-release-assets.py \
  smoke-daemon-semantic-release.sh; do
  cp -L "${repo_root}/scripts/${release_script}" \
    "${release_root}/scripts/${release_script}"
done
chmod 0755 "${release_root}/scripts/"*
cp -L "${repo_root}/contracts/release-targets-v1.json" \
  "${release_root}/contracts/release-targets-v1.json"
cp -L "${repo_root}/contracts/release-factory-inputs-v1.json" \
  "${release_root}/contracts/release-factory-inputs-v1.json"
cp -L "${repo_root}/scripts/release/build-public-candidate-on-linux.sh" \
  "${release_root}/scripts/release/build-public-candidate-on-linux.sh"
test -f "${release_root}/contracts/release-targets-v1.json"
test ! -L "${release_root}/contracts/release-targets-v1.json"
cat > "${tmp}/ubuntu-24.04-os-release" <<'EOF'
ID=ubuntu
VERSION_ID="24.04"
EOF
mv \
  "${release_root}/scripts/public-cli-host-runtime-evidence.sh" \
  "${release_root}/scripts/public-cli-host-runtime-evidence-real.sh"
host_runtime_evidence_real="${release_root}/scripts/public-cli-host-runtime-evidence-real.sh"
fake_sysctl="${tmp}/fake-sysctl"
cat > "${fake_sysctl}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${*: -1}" in
  sysctl.proc_translated) printf '0\n' ;;
  hw.optional.arm64) printf '1\n' ;;
  kern.hv_vmm_present) printf '1\n' ;;
  *) exit 1 ;;
esac
EOF
chmod 0755 "${fake_sysctl}"
fake_ioreg="${tmp}/fake-ioreg"
cat > "${fake_ioreg}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case " $* " in
  *' IOPlatformExpertDevice '*)
    printf '%s\n' "${CTX_TEST_DARWIN_PLATFORM_IDENTITY:-Apple Inc.}"
    ;;
  *' IOPCIDevice '*)
    printf '%s\n' "${CTX_TEST_DARWIN_DEVICE_SUMMARY:-Apple PCI device}"
    ;;
  *) exit 1 ;;
esac
EOF
chmod 0755 "${fake_ioreg}"
fake_system_profiler="${tmp}/fake-system-profiler"
cat > "${fake_system_profiler}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  SPHardwareDataType)
    printf 'Model Name: Apple Virtual Machine\nModel Identifier: VirtualMac\n'
    ;;
  SPDisplaysDataType)
    printf '%s\n' "${CTX_TEST_DARWIN_DISPLAY_SUMMARY:-Apple display}"
    ;;
  *) exit 1 ;;
esac
EOF
chmod 0755 "${fake_system_profiler}"

read_darwin_arm64_evidence() {
  "${host_runtime_evidence_real}" \
    --host-system Darwin \
    --host-arch arm64 \
    --sysctl "${fake_sysctl}" \
    --ioreg "${fake_ioreg}" \
    --system-profiler "${fake_system_profiler}"
}

expected_native_virtio_evidence=$'Darwin\tarm64\tarm64\t0\tsysctl\tapple\tnone\tpresent\t1'
for virtio_peripheral in 'VirtIO GPU' 'virtio-net' 'virtio_blk'; do
  actual_evidence="$(
    CTX_TEST_DARWIN_DEVICE_SUMMARY="${virtio_peripheral}" \
      read_darwin_arm64_evidence
  )"
  [[ "${actual_evidence}" == "${expected_native_virtio_evidence}" ]] || {
    printf 'native Apple arm64 VirtIO evidence was misclassified: %s\n' \
      "${actual_evidence}" >&2
    exit 1
  }
done

expected_explicit_emulation_evidence=$'Darwin\tarm64\tarm64\t0\tsysctl\tapple\tqemu-kvm\tpresent\t1'
for explicit_emulator in QEMU KVM TCG 'VMware SVGA' VirtualBox Parallels Bochs; do
  actual_evidence="$(
    CTX_TEST_DARWIN_DEVICE_SUMMARY="${explicit_emulator} VirtIO GPU" \
      read_darwin_arm64_evidence
  )"
  [[ "${actual_evidence}" == "${expected_explicit_emulation_evidence}" ]] || {
    printf 'explicit Darwin emulator evidence was not rejected: %s -> %s\n' \
      "${explicit_emulator}" "${actual_evidence}" >&2
    exit 1
  }
done

actual_evidence="$(
  CTX_TEST_DARWIN_PLATFORM_IDENTITY='Generic Platform' \
    CTX_TEST_DARWIN_DEVICE_SUMMARY='VirtIO GPU' \
    read_darwin_arm64_evidence
)"
expected_generic_virtio_evidence=$'Darwin\tarm64\tarm64\t0\tsysctl\tgeneric\tqemu-kvm\tpresent\t1'
[[ "${actual_evidence}" == "${expected_generic_virtio_evidence}" ]] || {
  printf 'generic Darwin VirtIO evidence became native: %s\n' \
    "${actual_evidence}" >&2
  exit 1
}

cat > "${release_root}/scripts/public-cli-host-runtime-evidence.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
for argument in "\$@"; do
  if [[ "\${argument}" == "--os-baseline-only" ]]; then
    if [[ "\${CTX_TEST_RUNTIME_EVIDENCE:-}" == macos-* ]]; then
      printf 'unknown\tunknown\tunknown\n'
      exit 0
    fi
    exec "${release_root}/scripts/public-cli-host-runtime-evidence-real.sh" \
      "\$@" --os-release "${tmp}/ubuntu-24.04-os-release"
  fi
done
if [[ "\${CTX_TEST_RUNTIME_EVIDENCE:-}" == "macos-arm64-native-virtualized" ]]; then
  printf 'Darwin\tarm64\tarm64\t0\tsysctl\tapple\tnone\tpresent\t1\n'
  exit 0
fi
if [[ "\${CTX_TEST_RUNTIME_EVIDENCE:-}" == "macos-arm64-generic-virtualized" ]]; then
  printf 'Darwin\tarm64\tarm64\t0\tsysctl\tgeneric\tnone\tpresent\t1\n'
  exit 0
fi
exec "${release_root}/scripts/public-cli-host-runtime-evidence-real.sh" "\$@"
EOF
chmod 0755 "${release_root}/scripts/public-cli-host-runtime-evidence.sh"
smoke="${release_root}/scripts/smoke-daemon-semantic-release.sh"
runtime_authority="${release_root}/scripts/public-cli-runtime-authority.sh"

expect_runtime_authority() {
  local name="$1"
  local expected="$2"
  local actual
  shift 2
  actual="$("${runtime_authority}" "$@")"
  if [[ "${actual}" != "${expected}" ]]; then
    printf 'unexpected runtime authority for %s: expected %s, got %s\n' \
      "${name}" "${expected}" "${actual}" >&2
    exit 1
  fi
}

expect_runtime_authority macos_arm64_bare_metal authoritative \
  macos-arm64 Darwin arm64 passed arm64 0 apple none absent 1
expect_runtime_authority macos_arm64_native_virtualized authoritative \
  macos-arm64 Darwin arm64 passed arm64 0 apple none present 1
expect_runtime_authority macos_arm64_rosetta non_authoritative \
  macos-arm64 Darwin arm64 passed arm64 1 apple rosetta-2 absent 1
expect_runtime_authority macos_arm64_emulated non_authoritative \
  macos-arm64 Darwin arm64 passed arm64 0 apple qemu-kvm present 1
expect_runtime_authority macos_arm64_generic_virtualized non_authoritative \
  macos-arm64 Darwin arm64 passed arm64 0 generic none present 1
expect_runtime_authority macos_arm64_incomplete non_authoritative \
  macos-arm64 Darwin arm64 passed arm64 0 apple none present 0
expect_runtime_authority macos_arm64_unknown_hypervisor non_authoritative \
  macos-arm64 Darwin arm64 passed arm64 0 apple none unknown 1
expect_runtime_authority macos_arm64_wrong_host_arch non_authoritative \
  macos-arm64 Darwin x86_64 passed arm64 0 apple none present 1
expect_runtime_authority macos_arm64_wrong_native_arch non_authoritative \
  macos-arm64 Darwin arm64 passed x86_64 0 apple none present 1
expect_runtime_authority macos_x64_apple_virtualized non_authoritative \
  macos-x64 Darwin x86_64 passed x86_64 0 apple none present 1

coreml_archive="${tmp}/ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz"
printf 'candidate Core ML bytes\n' > "${coreml_archive}"
printf 'candidate checksum sidecar\n' > "${coreml_archive}.sha256"
printf 'candidate asset record\n' > "${coreml_archive}.asset.json"
cat > "${release_root}/scripts/semantic-release-assets.py" <<'PY'
#!/usr/bin/env python3
import json
import os
import stat
import sys
from pathlib import Path

expected_archive = "ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz"
manifest_sha256 = "20a94162aca7c2f9f65be27839cd6867ec1c54e142fdf0c652de20139dffbc19"
if len(sys.argv) != 6 or sys.argv[1] != "bind-coreml-cache":
    raise SystemExit(f"unexpected candidate Core ML binder arguments: {sys.argv!r}")
values = dict(zip(sys.argv[2::2], sys.argv[3::2]))
if set(values) != {"--archive", "--cache-dir"}:
    raise SystemExit(f"unexpected candidate Core ML binder options: {values!r}")
archive = Path(values["--archive"])
cache = Path(values["--cache-dir"])
if archive.name != expected_archive or archive.read_bytes() != b"candidate Core ML bytes\n":
    raise SystemExit("wrong candidate Core ML archive")
if Path(f"{archive}.sha256").read_bytes() != b"candidate checksum sidecar\n":
    raise SystemExit("wrong candidate Core ML checksum sidecar")
if Path(f"{archive}.asset.json").read_bytes() != b"candidate asset record\n":
    raise SystemExit("wrong candidate Core ML asset record")
if stat.S_IMODE(cache.stat().st_mode) & 0o077 or any(cache.iterdir()):
    raise SystemExit("candidate Core ML cache was not clean and owner-private")
bundle = cache / "semantic-model-bundles" / "sha256" / manifest_sha256[:2] / manifest_sha256
bundle.mkdir(parents=True)
(bundle / "fixture-model").write_bytes(b"verified candidate Core ML model\n")
marker = bundle.with_name(f"{manifest_sha256}.complete.json")
marker.write_text(
    json.dumps(
        {"manifest_sha256": manifest_sha256, "schema_version": 1},
        separators=(",", ":"),
        sort_keys=True,
    )
    + "\n",
    encoding="ascii",
)
Path(os.environ["CTX_TEST_COREML_BIND_LOG"]).write_text(
    f"archive={archive}\ncache={cache}\n", encoding="utf-8"
)
print("archive_sha256=25fbf333d1e72f5c075973ef968dfa1446459f61f3ac63ef3690d9865435af17")
print(f"manifest_sha256={manifest_sha256}")
print(f"cache_bundle={bundle}")
PY
chmod 0755 "${release_root}/scripts/semantic-release-assets.py"

fake_ctx="${tmp}/ctx-macos-artifact"
cat > "${fake_ctx}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--version" ]]; then
  printf 'ctx 0.25.0\n'
  exit 0
fi

data_root=""
command=""
while (($# > 0)); do
  case "$1" in
    --data-root)
      data_root="${2:-}"
      shift 2
      ;;
    import|daemon|search)
      command="$1"
      shift
      break
      ;;
    *)
      printf 'unexpected fake ctx prefix argument: %s\n' "$1" >&2
      exit 1
      ;;
  esac
done

[[ -n "${data_root}" && -n "${command}" ]]
[[ "${CTX_INTERNAL_SEMANTIC_BACKEND:-}" == "coreml" ]]
[[ "${CTX_SEMANTIC_COREML_NATIVE_COMPUTE:-}" == "all" ]]
[[ "${CTX_DAEMON_ENABLED:-}" == "true" ]]
[[ "${CTX_SEARCH_SEMANTIC:-}" == "true" ]]
case "${CTX_INTERNAL_SEMANTIC_BACKEND:-}" in
  coreml)
    coreml_identity=20a94162aca7c2f9f65be27839cd6867ec1c54e142fdf0c652de20139dffbc19
    coreml_marker="${CTX_SEMANTIC_CACHE_DIR}/semantic-model-bundles/sha256/20/${coreml_identity}.complete.json"
    [[ -s "${coreml_marker}" ]] || {
      printf 'candidate Core ML cache was not bound before ctx execution\n' >&2
      exit 1
    }
    ;;
esac
case "${HOME}" in
  "${data_root}"|"${data_root}"/*)
    printf 'semantic smoke HOME overlaps the ctx data root\n' >&2
    exit 1
    ;;
esac
case "${CTX_SEMANTIC_CACHE_DIR:-}" in
  "${data_root}"|"${data_root}"/*)
    printf 'semantic smoke model cache overlaps the ctx data root\n' >&2
    exit 1
    ;;
esac

case "${command}" in
  import)
    for _ in {1..100}; do
      [[ -s "${data_root}/fake-daemon-pid" ]] && break
      sleep 0.01
    done
    [[ -s "${data_root}/fake-daemon-pid" ]] || {
      printf 'semantic smoke imported before launching its daemon\n' >&2
      exit 1
    }
    no_daemon=0
    fixture=""
    while (($# > 0)); do
      case "$1" in
        --path)
          fixture="${2:-}"
          shift 2
          ;;
        --no-daemon)
          no_daemon=1
          shift
          ;;
        *)
          shift
          ;;
      esac
    done
    [[ "${no_daemon}" == "1" ]] || {
      printf 'semantic smoke import could autostart an unowned daemon\n' >&2
      exit 1
    }
    [[ -f "${fixture}" ]]
    case "${fixture}" in
      "${data_root}"|"${data_root}"/*)
        printf 'semantic smoke fixture overlaps the ctx data root\n' >&2
        exit 1
        ;;
    esac
    grep -Eo 'ctx-release-semantic-smoke-[0-9a-f]+' "${fixture}" | head -1 \
      > "${data_root}/fake-marker"
    ;;
  daemon)
    subcommand="${1:-}"
    shift || true
    case "${subcommand}" in
      run)
        [[ -s "${data_root}/config.toml" ]] || {
          printf 'semantic smoke launched its daemon before writing config\n' >&2
          exit 1
        }
        printf '%s\n' "$$" > "${data_root}/fake-daemon-pid"
        mkdir -p "${data_root}/daemon"
        printf '{"schema_version":1,"pid":%s,"transport":"unix","path":"/tmp/fake-ctx-semantic-smoke.sock","token":"0123456789abcdef0123456789abcdef"}\n' "$$" \
          > "${data_root}/daemon/source-refresh-endpoint.json"
        trap 'exit 0' TERM INT
        while [[ ! -f "${data_root}/fake-marker" ]]; do sleep 0.05; done
        mkdir -p "${CTX_SEMANTIC_CACHE_DIR}/fake-verified-model"
        printf 'daemon-owned verified model\n' \
          > "${CTX_SEMANTIC_CACHE_DIR}/fake-verified-model/complete"
        while :; do sleep 1; done
        ;;
      status)
        pid="$(cat "${data_root}/fake-daemon-pid")"
        if [[ -s "${data_root}/fake-marker" && \
              -s "${CTX_SEMANTIC_CACHE_DIR}/fake-verified-model/complete" ]]; then
          semantic_status=ready
          indexed_chunks=1
        else
          semantic_status=pending
          indexed_chunks=0
        fi
        printf '{"daemon":{"pid":%s,"status":"running","running":true,"jobs":{"semantic_index":{"status":"%s","model_key":"e5-small-v1:mean-pool:l2:query-passage","indexed_chunks":%s}}}}\n' \
          "${pid}" "${semantic_status}" "${indexed_chunks}"
        ;;
      *)
        printf 'unexpected fake daemon command: %s\n' "${subcommand}" >&2
        exit 1
        ;;
    esac
    ;;
  search)
    marker="$(cat "${data_root}/fake-marker")"
    printf '{"retrieval":{"requested_mode":"semantic","effective_mode":"semantic","semantic_status":"ready"},"results":[{"text":"%s"}]}\n' "${marker}"
    ;;
esac
EOF
chmod 755 "${fake_ctx}"
fake_ctx="$(cd -- "$(dirname -- "${fake_ctx}")" && pwd -P)/$(basename -- "${fake_ctx}")"

expect_usage_failure() {
  local name="$1"
  local expected="$2"
  shift 2
  if "${smoke}" "$@" > "${tmp}/${name}.out" 2> "${tmp}/${name}.err"; then
    printf 'expected argument failure: %s\n' "${name}" >&2
    exit 1
  fi
  grep -Fq -- "${expected}" "${tmp}/${name}.err" || {
    printf 'unexpected argument failure for %s\n' "${name}" >&2
    cat "${tmp}/${name}.err" >&2
    exit 1
  }
}

"${smoke}" --help > "${tmp}/help.out" 2>&1
grep -Fq -- '--coreml --runtime-platform macos-arm64|macos-x64' "${tmp}/help.out"
grep -Fq -- '[--coreml-archive PATH]' "${tmp}/help.out"
grep -Fq -- '--require-authoritative' "${tmp}/help.out"

expect_usage_failure coreml_linux \
  '--coreml requires --runtime-platform macos-arm64 or macos-x64' \
  --coreml --runtime-platform linux-x64 --ctx "${fake_ctx}"
expect_usage_failure coreml_archive \
  '--coreml cannot be combined with --runtime-archive' \
  --coreml --runtime-platform macos-arm64 --runtime-archive "${tmp}/unused" \
  --ctx "${fake_ctx}"
expect_usage_failure coreml_candidate_without_mode \
  '--coreml-archive requires --coreml' \
  --runtime-platform macos-arm64 --coreml-archive "${coreml_archive}" \
  --ctx "${fake_ctx}"
expect_usage_failure coreml_candidate_x64 \
  '--coreml-archive requires --runtime-platform macos-arm64' \
  --coreml --runtime-platform macos-x64 --coreml-archive "${coreml_archive}" \
  --ctx "${fake_ctx}"
expect_usage_failure archive_required \
  '--runtime-archive is required unless --coreml is selected' \
  --runtime-platform macos-arm64 --ctx "${fake_ctx}"
expect_usage_failure retired_proof_output \
  'Usage:' \
  --coreml --runtime-platform macos-arm64 --proof-output "${tmp}/proof" \
  --ctx "${fake_ctx}"

if CTX_TEST_RUNTIME_EVIDENCE=macos-arm64-generic-virtualized "${smoke}" \
  --coreml \
  --runtime-platform macos-arm64 \
  --ctx "${fake_ctx}" \
  --data-root "${tmp}/non-authoritative-runs" \
  --require-authoritative \
  --timeout-seconds 30 \
  > "${tmp}/non-authoritative.out" 2> "${tmp}/non-authoritative.err"; then
  printf 'generic virtualized macOS arm64 smoke unexpectedly passed\n' >&2
  exit 1
fi
expected_authority_diagnostic="ctx semantic smoke: runtime authority evidence: platform=macos-arm64 host_system=Darwin host_arch=arm64 runtime_status=passed host_native_arch=arm64 process_translated=0 native_arch_probe=sysctl hardware_identity=generic emulation=none hypervisor=present evidence_complete=1 runner_id='' os_identity=unknown os_version=unknown os_product_type=unknown runtime_os_baseline=ubuntu-24.04 authority=non_authoritative"
grep -Fqx -- "${expected_authority_diagnostic}" \
  "${tmp}/non-authoritative.out" || {
  printf 'missing full non-authoritative runtime evidence diagnostic\n' >&2
  cat "${tmp}/non-authoritative.out" >&2
  exit 1
}
grep -Fq -- \
  'error: semantic smoke requires authoritative native macos-arm64 execution' \
  "${tmp}/non-authoritative.err"

run_parent="${tmp}/runs"
coreml_bind_log="${tmp}/coreml-bind.log"
CTX_TEST_RUNTIME_EVIDENCE=macos-arm64-native-virtualized \
CTX_TEST_COREML_BIND_LOG="${coreml_bind_log}" "${smoke}" \
  --coreml \
  --runtime-platform macos-arm64 \
  --coreml-archive "${coreml_archive}" \
  --ctx "${fake_ctx}" \
  --data-root "${run_parent}" \
  --require-authoritative \
  --timeout-seconds 30 \
  --keep-root \
  > "${tmp}/coreml.out" 2> "${tmp}/coreml.err"

run_root="$(find "${run_parent}" -mindepth 1 -maxdepth 1 -type d -name 'ctx-semantic-smoke.*' -print -quit)"
[[ -n "${run_root}" ]]
test ! -e "${run_root}/data/packaged-runtime-proof.txt"
grep -Fq 'ctx semantic smoke ok:' "${tmp}/coreml.out"
grep -Fq \
  'hardware_identity=apple emulation=none hypervisor=present evidence_complete=1' \
  "${tmp}/coreml.out"
grep -Fq 'authority=authoritative' "${tmp}/coreml.out"
grep -Fxq -- "archive=${coreml_archive}" "${coreml_bind_log}"
grep -Fq -- '/semantic-cache' "${coreml_bind_log}"
grep -Fq -- \
  'archive_sha256=25fbf333d1e72f5c075973ef968dfa1446459f61f3ac63ef3690d9865435af17' \
  "${tmp}/coreml.out"
[[ ! -e "${run_root}/data/runtime/onnxruntime" ]]
python3 -I - "${run_root}/installed/bin" <<'PY'
import os
import stat
import sys

path = sys.argv[1]
mode = stat.S_IMODE(os.stat(path).st_mode)
if mode & 0o022:
    raise SystemExit(f"semantic smoke executable directory is not owner-safe: {mode:o}")
PY

daemon_pid="$(cat "${run_root}/data/fake-daemon-pid")"
if kill -0 "${daemon_pid}" >/dev/null 2>&1; then
  printf 'CoreML smoke left daemon process %s running\n' "${daemon_pid}" >&2
  exit 1
fi

cpu_ctx="${tmp}/ctx-linux-cpu"
sed \
  -e 's/== "coreml"/== "cpu"/' \
  -e '/CTX_SEMANTIC_COREML_NATIVE_COMPUTE/d' \
  -e 's/"backend":"coreml","compute_mode":"all"/"backend":"cpu","preference":"cpu"/g' \
  "${fake_ctx}" > "${cpu_ctx}"
chmod 755 "${cpu_ctx}"
printf 'synthetic lock\n' > "${tmp}/Cargo.lock"
recipe="${release_root}/scripts/release/build-public-candidate-on-linux.sh"
python3 - \
  "${cpu_ctx}" \
  "${tmp}/Cargo.lock" \
  "${release_root}/contracts/release-targets-v1.json" \
  "${release_root}/contracts/release-factory-inputs-v1.json" \
  "${recipe}" <<'PY'
import hashlib
import json
from pathlib import Path
import re
import sys

artifact, cargo_lock, matrix_path, inputs_path, recipe = map(Path, sys.argv[1:])
matrix = json.loads(matrix_path.read_text(encoding="utf-8"))
inputs = json.loads(inputs_path.read_text(encoding="utf-8"))
target = next(item for item in matrix["targets"] if item["id"] == "linux-x64")
host = inputs["linux_host"]
pins = dict(re.findall(
    r'^readonly (RUST_VERSION|RUST_COMMIT|ZIG_VERSION|CARGO_ZIGBUILD_VERSION)="([^"\n]+)"$',
    recipe.read_text(encoding="utf-8"),
    re.MULTILINE,
))
if set(pins) != {
    "RUST_VERSION",
    "RUST_COMMIT",
    "ZIG_VERSION",
    "CARGO_ZIGBUILD_VERSION",
}:
    raise SystemExit("factory recipe toolchain pins are incomplete")

def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()

value = {
    "artifact_sha256": sha256(artifact),
    "build_system": "cargo-zigbuild",
    "builder": {
        "authority": host["authority"],
        "image_id": None,
        "os": f'{host["os_id"]}-{host["os_version"]}-{host["arch"]}',
        "recipe_sha256": sha256(recipe),
    },
    "cargo_lock_sha256": sha256(cargo_lock),
    "gates": {
        "local_runtime": "not_run",
        "local_runtime_authority": "not_run",
        "static": "passed",
        "static_abi": "passed",
    },
    "inspector": {
        "authority": "ctx-release-static-llvm-v1",
        "image_id": None,
        "tool": "LLVM version 20",
    },
    "linux_build": target["linux_build"],
    "platform": "linux-x64",
    "release_factory": {
        "authority": target["public_construction_authority"],
        "cargo_zigbuild_version": pins["CARGO_ZIGBUILD_VERSION"],
        "macos_sdk_authority": None,
        "macos_sdk_sha256": None,
        "zig_version": pins["ZIG_VERSION"],
    },
    "representative_cpu_proof": {"profile": None, "qemu_version": None},
    "runtime": {"authority": "native-fanout-deferred-v1", "image_id": None},
    "rust_version": (
        f'rustc {pins["RUST_VERSION"]} ({pins["RUST_COMMIT"][:9]} 2026-07-01)'
    ),
    "schema_version": 1,
    "source": {
        "clean": True,
        "commit": "0123456789abcdef0123456789abcdef01234567",
    },
    "target": target["public_rust_target"],
}
artifact.with_name(f"{artifact.name}.build-info.json").write_text(
    json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
PY

runtime_payload="${tmp}/runtime-payload"
mkdir -p "${runtime_payload}/lib"
printf 'license\n' > "${runtime_payload}/LICENSE"
printf 'notices\n' > "${runtime_payload}/ThirdPartyNotices.txt"
printf '1.27.0\n' > "${runtime_payload}/VERSION_NUMBER"
printf 'synthetic-commit\n' > "${runtime_payload}/GIT_COMMIT_ID"
printf 'synthetic runtime\n' > "${runtime_payload}/lib/libonnxruntime.so"
runtime_archive="${tmp}/ctx-onnxruntime-linux-x64.tar.gz"
tar --no-recursion -C "${runtime_payload}" -czf "${runtime_archive}" \
  LICENSE ThirdPartyNotices.txt VERSION_NUMBER GIT_COMMIT_ID lib lib/libonnxruntime.so
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "${runtime_archive}" | awk '{ print $1 }' > "${runtime_archive}.sha256"
else
  shasum -a 256 "${runtime_archive}" | awk '{ print $1 }' > "${runtime_archive}.sha256"
fi

if ! "${smoke}" \
  --runtime-archive "${runtime_archive}" \
  --runtime-platform linux-x64 \
  --ctx "${cpu_ctx}" \
  --data-root "${tmp}/onnx-runs" \
  --require-authoritative \
  --timeout-seconds 30 \
  > "${tmp}/onnx.out" 2> "${tmp}/onnx.err"; then
  cat "${tmp}/onnx.out" >&2
  cat "${tmp}/onnx.err" >&2
  exit 1
fi
grep -Fq 'ctx semantic smoke ok:' "${tmp}/onnx.out"
if find "${tmp}/onnx-runs" -name packaged-runtime-proof.txt -print -quit | grep -q .; then
  printf 'semantic smoke emitted a retired proof artifact\n' >&2
  exit 1
fi

candidate="${tmp}/nested-inputs"
mkdir "${candidate}"
cp "${cpu_ctx}" "${candidate}/ctx"
chmod 0755 "${candidate}/ctx"
cp "${cpu_ctx}.build-info.json" "${candidate}/ctx.build-info.json"
cp "${runtime_archive}" "${candidate}/ctx-onnxruntime-linux-x64.tar.gz"
cp "${runtime_archive}.sha256" \
  "${candidate}/ctx-onnxruntime-linux-x64.tar.gz.sha256"
printf 'ctx 0.25.0\n' > "${candidate}/ctx.version"
mkdir -p "${release_root}/tests/fixtures/custom-history-jsonl"
printf '{}\n' > \
  "${release_root}/tests/fixtures/custom-history-jsonl/basic.jsonl"
cat > "${release_root}/scripts/run-native-candidate-smoke.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
ctx_path="$1"
fixture="$2"
expected_version="$3"
output="$4"
[[ -x "${ctx_path}" && -s "${fixture}" && "${expected_version}" == "0.25.0" ]]
[[ "$("${ctx_path}" --version)" == "ctx ${expected_version}" ]]
printf '{"status":"passed"}\n' > "${output}"
EOF
chmod 0755 "${release_root}/scripts/run-native-candidate-smoke.sh"
nested_smoke_root="${tmp}/nested-native-smoke"
mkdir -p "${nested_smoke_root}"
if ! bash -ceu '
    ctx_path="$1"
    runtime="$2"
    version_file="$3"
    smoke_root="$4"
    execution_root="$5"
    cd "${execution_root}"
    expected_version="$(sed -n "s/^ctx //p" "${version_file}")"
    [[ -n "${expected_version}" ]]
    scripts/run-native-candidate-smoke.sh \
      "${ctx_path}" \
      tests/fixtures/custom-history-jsonl/basic.jsonl \
      "${expected_version}" \
      "${smoke_root}/candidate-smoke.json"
    scripts/smoke-daemon-semantic-release.sh \
      --runtime-archive "${runtime}" \
      --runtime-platform linux-x64 \
      --ctx "${ctx_path}" \
      --data-root "${smoke_root}" \
      --require-authoritative \
      --timeout-seconds 30
  ' bash \
    "${candidate}/ctx" \
    "${candidate}/ctx-onnxruntime-linux-x64.tar.gz" \
    "${candidate}/ctx.version" "${nested_smoke_root}" "${release_root}" \
  > "${tmp}/nested-onnx.out" 2> "${tmp}/nested-onnx.err"; then
  cat "${tmp}/nested-onnx.out" >&2
  cat "${tmp}/nested-onnx.err" >&2
  exit 1
fi
grep -Fq 'ctx semantic smoke ok:' "${tmp}/nested-onnx.out"
grep -Fq '"status":"passed"' \
  "${nested_smoke_root}/candidate-smoke.json"

printf 'daemon semantic release smoke contract tests passed\n'
