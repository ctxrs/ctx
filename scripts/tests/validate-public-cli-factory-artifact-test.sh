#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  source_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  source_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fi
test_root="$(mktemp -d)"
trap 'rm -rf "${test_root}"' EXIT

repo_root="${test_root}/repo"
artifact_dir="${repo_root}/-artifacts"
command_dir="${test_root}/commands"
output_dir="${test_root}/output"
mkdir -p \
  "${repo_root}/contracts" \
  "${repo_root}/scripts/release" \
  "${repo_root}/scripts" \
  "${artifact_dir}" \
  "${command_dir}"
cp "${source_root}/Cargo.lock" "${repo_root}/Cargo.lock"
cp \
  "${source_root}/contracts/release-factory-inputs-v1.json" \
  "${source_root}/contracts/release-targets-v1.json" \
  "${repo_root}/contracts/"
cp \
  "${source_root}/scripts/check-public-cli-build-info.py" \
  "${source_root}/scripts/validate-public-cli-factory-artifact.sh" \
  "${repo_root}/scripts/"
cp \
  "${source_root}/scripts/release/build-public-candidate-on-linux.sh" \
  "${repo_root}/scripts/release/"
validator="${repo_root}/scripts/validate-public-cli-factory-artifact.sh"

artifact="${artifact_dir}/ctx"
printf '#!/bin/sh\nexit 1\n' >"${artifact}"
chmod 600 "${artifact}"
sha256_file "${artifact}" >"${artifact}.sha256"

ARTIFACT="${artifact}" REPO_ROOT="${repo_root}" python3 - <<'PY'
import hashlib
import json
import os
from pathlib import Path
import re

artifact = Path(os.environ["ARTIFACT"])
root = Path(os.environ["REPO_ROOT"])
matrix = json.loads((root / "contracts/release-targets-v1.json").read_text())
target = next(item for item in matrix["targets"] if item["id"] == "linux-x64")
recipe = root / target["public_construction_label"]
recipe_source = recipe.read_text()
pins = dict(re.findall(
    r'^readonly (RUST_VERSION|RUST_COMMIT|ZIG_VERSION|CARGO_ZIGBUILD_VERSION)="([^"\n]+)"$',
    recipe_source,
    re.MULTILINE,
))
source = {"clean": True, "commit": "a" * 40}
build_info = {
    "artifact_sha256": hashlib.sha256(artifact.read_bytes()).hexdigest(),
    "build_system": "cargo-zigbuild",
    "builder": {
        "authority": "ctx-release-factory-ubuntu24-x86_64-v1",
        "image_id": None,
        "os": "ubuntu-24.04-x86_64",
        "recipe_sha256": hashlib.sha256(recipe.read_bytes()).hexdigest(),
    },
    "cargo_lock_sha256": hashlib.sha256((root / "Cargo.lock").read_bytes()).hexdigest(),
    "gates": {
        "local_runtime": "not_run",
        "local_runtime_authority": "not_run",
        "static": "passed",
        "static_abi": "passed",
    },
    "inspector": {
        "authority": "ctx-release-static-llvm-v1",
        "image_id": None,
        "tool": "llvm",
    },
    "linux_build": target["linux_build"],
    "platform": "linux-x64",
    "release_factory": {
        "authority": "linux-cross-cargo-zigbuild-v1",
        "cargo_zigbuild_version": pins["CARGO_ZIGBUILD_VERSION"],
        "macos_sdk_authority": None,
        "macos_sdk_sha256": None,
        "zig_version": pins["ZIG_VERSION"],
    },
    "representative_cpu_proof": {"profile": None, "qemu_version": None},
    "runtime": {"authority": "native-fanout-deferred-v1", "image_id": None},
    "rust_version": (
        f"rustc {pins['RUST_VERSION']} ({pins['RUST_COMMIT'][:9]} 2026-07-01)"
    ),
    "schema_version": 1,
    "source": source,
    "target": target["public_rust_target"],
}
build_info_path = artifact.with_name("ctx.build-info.json")
build_info_bytes = (json.dumps(build_info, sort_keys=True, separators=(",", ":")) + "\n").encode()
build_info_path.write_bytes(build_info_bytes)
artifact_sha256 = hashlib.sha256(artifact.read_bytes()).hexdigest()
candidate = {
    "schema_version": 1,
    "kind": "ctx-public-cli-candidate",
    "construction": {
        "authority": "linux-cross-cargo-zigbuild-v1",
        "label": "scripts/release/build-public-candidate-on-linux.sh",
    },
    "product": "core",
    "version": "1.0.0",
    "target": {
        "id": "linux-x64",
        "platform": "linux-x64",
        "rust_triple": target["public_rust_target"],
    },
    "source": source,
    "artifact": {
        "file": artifact.name,
        "sha256": artifact_sha256,
        "size_bytes": artifact.stat().st_size,
    },
    "evidence": {
        "build_info": {
            "file": build_info_path.name,
            "sha256": hashlib.sha256(build_info_bytes).hexdigest(),
        }
    },
    "tantivy": {},
}
artifact.with_name("ctx.candidate.json").write_text(
    json.dumps(candidate, sort_keys=True, separators=(",", ":")) + "\n"
)
artifact.with_name("ctx.version").write_text("ctx 1.0.0\n")
PY

cat >"${command_dir}/git" <<'SH'
#!/bin/sh
if [ "$*" = "rev-parse --verify HEAD^{commit}" ]; then
  printf '%s\n' aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
  exit 0
fi
exit 64
SH

cat >"${command_dir}/cargo" <<'SH'
#!/bin/sh
: >"${CTX_TEST_CARGO_MARKER}"
exit 99
SH

cat >"${command_dir}/chmod" <<'SH'
#!/bin/sh
[ "$#" -eq 2 ] || exit 64
[ "$1" = u+x ] || exit 64
printf '%s\n' "$1" "$2" >>"${CTX_TEST_CHMOD_LOG}"
exec "${CTX_TEST_REAL_CHMOD}" "$@"
SH

real_chmod="$(command -v chmod)"
chmod u+x \
  "${command_dir}/git" \
  "${command_dir}/cargo" \
  "${command_dir}/chmod"

chmod_log="${test_root}/chmod.log"
cargo_marker="${test_root}/cargo-called"
checker=(
  python3 -I "${repo_root}/scripts/check-public-cli-build-info.py"
  --artifact "${artifact}"
  --build-info "${artifact}.build-info.json"
  --matrix "${repo_root}/contracts/release-targets-v1.json"
  --platform linux-x64
  --source-commit aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
  --cargo-lock "${repo_root}/Cargo.lock"
  --factory-inputs "${repo_root}/contracts/release-factory-inputs-v1.json"
  --candidate-manifest "${artifact}.candidate.json"
  --version-file "${artifact}.version"
)
PATH="${command_dir}:/usr/bin:/bin" \
  CTX_TEST_CARGO_MARKER="${cargo_marker}" \
  "${checker[@]}" >"${test_root}/version"
[[ "$(cat "${test_root}/version")" == 1.0.0 ]] || \
  fail "checker rejected the exact immutable Linux factory contract"
cp "${artifact}.build-info.json" "${test_root}/exact-build-info.json"
BUILD_INFO="${artifact}.build-info.json" python3 - <<'PY'
import json
import os
from pathlib import Path

path = Path(os.environ["BUILD_INFO"])
value = json.loads(path.read_text())
value["linux_build"] = None
path.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n")
PY
if PATH="${command_dir}:/usr/bin:/bin" \
  CTX_TEST_CARGO_MARKER="${cargo_marker}" \
  "${checker[@]}" >"${test_root}/null-stdout" 2>"${test_root}/null-stderr"; then
  fail "checker accepted a null Linux ABI contract"
fi
grep -Fq "does not match the matrix ABI contract" "${test_root}/null-stderr" || \
  fail "checker rejected null Linux metadata for an unexpected reason"
cp "${test_root}/exact-build-info.json" "${artifact}.build-info.json"

set +e
PATH="${command_dir}:/usr/bin:/bin" \
  CTX_TEST_REAL_CHMOD="${real_chmod}" \
  CTX_TEST_CHMOD_LOG="${chmod_log}" \
  CTX_TEST_CARGO_MARKER="${cargo_marker}" \
  bash "${validator}" linux-x64 -artifacts "${output_dir}" \
  >"${test_root}/stdout" 2>"${test_root}/stderr"
status=$?
set -e

[[ ${status} -ne 0 ]] || fail "fake candidate unexpectedly completed validation"
if grep -Fq "factory artifact directory is unavailable" "${test_root}/stderr"; then
  fail "validator parsed a leading-dash artifact directory as an option"
fi
[[ -x "${artifact}" ]] || fail "validator did not restore owner execute permission"
[[ -f "${chmod_log}" ]] || fail "BSD-compatible chmod shim was not invoked"
[[ ! -e "${cargo_marker}" ]] || fail "validator invoked Cargo"
[[ "$(cat "${artifact}.sha256")" == "$(sha256_file "${artifact}")" ]] || \
  fail "mode restoration changed artifact bytes"
[[ "$(sed -n '1p' "${chmod_log}")" == u+x ]] || \
  fail "validator passed a non-portable chmod mode"
expected_artifact="$(cd "${artifact_dir}" && pwd -P)/ctx"
[[ "$(sed -n '2p' "${chmod_log}")" == "${expected_artifact}" ]] || \
  fail "validator did not pass the canonical artifact path to chmod"
[[ "$(wc -l <"${chmod_log}" | tr -d '[:space:]')" == 2 ]] || \
  fail "validator passed an extra chmod operand"
if grep -Fq "could not establish factory artifact executable mode" \
  "${test_root}/stderr"; then
  fail "validator rejected the BSD-compatible chmod path"
fi

set +e
bash "${validator}" linux-x64 -artifacts "${output_dir}" \
  companion-artifact managed-pair-envelope \
  >"${test_root}/pair-mutation-stdout" 2>"${test_root}/pair-mutation-stderr"
pair_mutation_status=$?
set -e
[[ ${pair_mutation_status} -eq 2 ]] || \
  fail "validator did not reject a companion/pair-envelope arity mutation"
grep -Fxq \
  'Usage: scripts/validate-public-cli-factory-artifact.sh PLATFORM ARTIFACT_DIR OUTPUT_DIR' \
  "${test_root}/pair-mutation-stderr" || \
  fail "validator did not preserve the exact Core-only three-argument usage"

printf 'PASS: immutable Core factory identity, exact arity, and BSD/GNU mode restoration\n'
