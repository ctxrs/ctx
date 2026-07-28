#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  source_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  source_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fi
test_root="$(
  mktemp -d "${TEST_TMPDIR:-${TMPDIR:-/tmp}}/ctx-public-bazel-release-test.XXXXXX"
)"
cleanup() {
  rm -rf -- "${test_root}"
}
trap cleanup EXIT

repo="${test_root}/repo"
runfiles="${test_root}/runfiles"
route_runfiles="${runfiles}/_main/ctx_release_routes/linux-x64"
mkdir -p \
  "${repo}/scripts" \
  "${repo}/scripts/release" \
  "${repo}/contracts" \
  "${repo}/crates/ctx-cli" \
  "${repo}/crates/ctx-history-index" \
  "${repo}/tests/fixtures/custom-history-jsonl" \
  "${repo}/bazel-out/k8-opt/bin/crates/ctx-cli" \
  "${repo}/inputs" \
  "${route_runfiles}"

cp "${source_root}/contracts/release-targets-v1.json" "${repo}/contracts/"
cp "${source_root}/contracts/release-candidate-manifest-v1.schema.json" \
  "${repo}/contracts/"
cp "${source_root}/scripts/check-release-target-matrix.py" "${repo}/scripts/"
cp "${source_root}/scripts/public-cli-release-targets.py" "${repo}/scripts/"
cp "${source_root}/scripts/write-public-cli-build-info.py" "${repo}/scripts/"
cp "${source_root}/scripts/check-public-cli-build-info.py" "${repo}/scripts/"
cp "${source_root}/scripts/install-public-cli-candidate.py" "${repo}/scripts/"
cp "${source_root}/scripts/release/public-cli-bazel-build-info.py" \
  "${repo}/scripts/release/"
cp "${source_root}/scripts/release/linux-bazel-release.Dockerfile" \
  "${repo}/scripts/release/"
cp "${source_root}/.bazelversion" "${repo}/.bazelversion"
cp "${source_root}/Cargo.lock" "${repo}/Cargo.lock"
cp "${source_root}/Cargo.toml" "${repo}/Cargo.toml"
cp "${source_root}/MODULE.bazel" "${repo}/MODULE.bazel"
cp "${source_root}/MODULE.bazel.lock" "${repo}/MODULE.bazel.lock"
cp "${source_root}/crates/ctx-cli/Cargo.toml" "${repo}/crates/ctx-cli/Cargo.toml"
cp "${source_root}/crates/ctx-history-index/Cargo.toml" \
  "${repo}/crates/ctx-history-index/Cargo.toml"
cp "${source_root}/tests/fixtures/custom-history-jsonl/basic.jsonl" \
  "${repo}/tests/fixtures/custom-history-jsonl/basic.jsonl"

cat >"${repo}/Cargo.lock" <<'EOF'
version = 4

[[package]]
name = "ctx"
version = "0.26.0"
dependencies = [
 "dependency 1.2.3 (registry+https://github.com/rust-lang/crates.io-index)",
]

[[package]]
name = "dependency"
version = "1.2.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
EOF

cat >"${repo}/.gitignore" <<'EOF'
bazel-out/
inputs/
out-*/
__pycache__/
EOF
printf 'tracked\n' >"${repo}/tracked.txt"

cat >"${repo}/scripts/release-sbom.py" <<'PY'
#!/usr/bin/env python3
from pathlib import Path
import sys


def option(name: str) -> Path:
    index = sys.argv.index(name)
    return Path(sys.argv[index + 1])


mode = sys.argv[1]
if mode == "generate":
    for flag, payload in (
        ("--output", b'{"bomFormat":"CycloneDX"}\n'),
        ("--notices-output", b"synthetic third-party notices\n"),
        ("--size-report-output", b'{"size_bytes":1}\n'),
        ("--candidate-manifest", b'{"kind":"ctx-public-cli-candidate"}\n'),
    ):
        option(flag).write_bytes(payload)
elif mode == "verify":
    for flag in ("--sbom", "--notices", "--size-report", "--candidate-manifest"):
        if not option(flag).is_file():
            raise SystemExit(f"missing {flag}")
elif mode == "verify-bundle":
    for flag in (
        "--artifact",
        "--build-info",
        "--sbom",
        "--notices",
        "--size-report",
        "--candidate-manifest",
    ):
        if not option(flag).is_file():
            raise SystemExit(f"missing {flag}")
else:
    raise SystemExit(f"unexpected mode: {mode}")
print("0" * 64)
PY

cat >"${repo}/scripts/check-public-cli-artifact.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
platform="$1"
directory="$2"
[[ "${platform}" == "linux-x64" ]]
artifact="${directory}/ctx"
[[ -s "${artifact}" && -s "${artifact}.sha256" && -s "${artifact}.version" ]]
[[ "$(sha256sum "${artifact}" | awk '{print $1}')" == "$(cat "${artifact}.sha256")" ]]
if [[ "${CTX_FAKE_MUTATE_ARTIFACT:-0}" == "1" ]]; then
  printf 'changed\n' >>"${artifact}"
fi
if [[ "${CTX_FAKE_MUTATE_SOURCE:-0}" == "1" ]]; then
  printf 'changed\n' >>"${BUILD_WORKSPACE_DIRECTORY}/tracked.txt"
fi
if [[ -n "${CTX_FAKE_COLLISION_PATH:-}" ]]; then
  mkdir -p "$(dirname "${CTX_FAKE_COLLISION_PATH}")"
  printf 'hostile\n' >"${CTX_FAKE_COLLISION_PATH}"
fi
EOF

cat >"${repo}/scripts/run-native-candidate-smoke.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '{"status":"passed"}\n' >"$4"
EOF

cat >"${repo}/scripts/public-cli-host-runtime-evidence.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'Linux\tx86_64\tx86_64\t0\tuname\tgeneric\tnone\tabsent\t1\n'
EOF

cat >"${repo}/scripts/public-cli-runtime-authority.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'authoritative\n'
EOF

for hook in \
  check-macos-release-signing.sh \
  run-macos-release-signing.sh \
  verify-macos-signed-cli.sh; do
  cat >"${repo}/scripts/${hook}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
exit 99
EOF
done

cat >"${repo}/bazel-out/k8-opt/bin/crates/ctx-cli/ctx" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  _release-build-identity)
    printf 'CTX_RELEASE_BUILD_SOURCE_COMMIT=@SOURCE_COMMIT@\n'
    printf 'CTX_RELEASE_BUILD_CARGO_LOCK_SHA256=@CARGO_LOCK_SHA256@\n'
    printf 'CTX_RELEASE_BUILD_TARGET=x86_64-unknown-linux-gnu\n'
    ;;
  --version)
    printf 'ctx 0.26.0\n'
    ;;
  *)
    exit 1
    ;;
esac
EOF

cat >"${repo}/inputs/rustc" <<'EOF'
#!/usr/bin/env sh
printf 'rustc 1.97.1 (8bab26f4f 2026-07-10)\n'
EOF

chmod 0755 \
  "${repo}/scripts/"*.sh \
  "${repo}/bazel-out/k8-opt/bin/crates/ctx-cli/ctx" \
  "${repo}/inputs/rustc"

git -C "${repo}" init -q
git -C "${repo}" config user.email ctx-release-test@example.invalid
git -C "${repo}" config user.name "ctx release test"
git -C "${repo}" add .
git -C "${repo}" commit -qm baseline
source_commit="$(git -C "${repo}" rev-parse HEAD)"
artifact="${repo}/bazel-out/k8-opt/bin/crates/ctx-cli/ctx"
python3 - "${artifact}" "${source_commit}" "$(sha256sum "${repo}/Cargo.lock" | awk '{print $1}')" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
value = path.read_text(encoding="utf-8")
value = value.replace("@SOURCE_COMMIT@", sys.argv[2])
value = value.replace("@CARGO_LOCK_SHA256@", sys.argv[3])
path.write_text(value, encoding="utf-8")
PY
chmod 0755 "${artifact}"
build_info="${repo}/inputs/linux-x64.build-info.json"
builder_digest="sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982"
builder_recipe_sha256="$(
  sha256sum "${repo}/scripts/release/linux-bazel-release.Dockerfile" \
    | awk '{print $1}'
)"

python3 "${repo}/scripts/write-public-cli-build-info.py" \
  --output "${build_info}" \
  --artifact "${artifact}" \
  --cargo-lock "${repo}/Cargo.lock" \
  --platform linux-x64 \
  --target x86_64-unknown-linux-gnu \
  --source-commit "${source_commit}" \
  --source-clean true \
  --rust-version "rustc 1.97.1 (8bab26f4f 2026-07-10)" \
  --expected-builder-base "${builder_digest}" \
  --actual-builder-base "${builder_digest}" \
  --builder-image-id "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" \
  --builder-recipe-sha256 "${builder_recipe_sha256}" \
  --runtime-image-id "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc" \
  --inspector-image-id "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd" \
  --linux-builder-image "docker.io/library/ubuntu:22.04@${builder_digest}" \
  --linux-ubuntu-snapshot 20260701T000000Z \
  --linux-glibc-max 2.35 \
  --linux-rust-toolchain 1.97.1 \
  --linux-rust-commit 8bab26f4f68e0e26f0bb7960be334d5b520ea452 \
  --linux-rust-sysroot /opt/rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu \
  --static-status passed \
  --local-runtime-status passed \
  --local-runtime-authority authoritative
python3 - "${build_info}" "${repo}" <<'PY'
import hashlib
import json
from pathlib import Path
import sys

path = Path(sys.argv[1])
repo = Path(sys.argv[2])
value = json.loads(path.read_bytes())
value["build_system"] = "bazel"
value["release_version"] = "0.26.0"
value["bazel"] = {
    "module_file_sha256": hashlib.sha256(
        (repo / "MODULE.bazel").read_bytes()
    ).hexdigest(),
    "module_lock_sha256": hashlib.sha256(
        (repo / "MODULE.bazel.lock").read_bytes()
    ).hexdigest(),
    "release_target_matrix_sha256": hashlib.sha256(
        (repo / "contracts/release-targets-v1.json").read_bytes()
    ).hexdigest(),
    "rustc_version": "rustc 1.97.1 (8bab26f4f 2026-07-10)",
    "version": (repo / ".bazelversion").read_text(encoding="ascii").strip(),
}
path.write_text(
    json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
PY

ln -s "${artifact}" "${route_runfiles}/artifact"
ln -s "${repo}/inputs/rustc" "${route_runfiles}/rustc"
cat >"${repo}/inputs/sbom-inventory.txt" <<'EOF'
//crates/ctx-cli:ctx
@@rules_rust~~crate~crates__dependency-1.2.3//:dependency
EOF
cat >"${repo}/inputs/license-materials.txt" <<'EOF'
main	_main/Cargo.toml
main	_main/crates/ctx-cli/Cargo.toml
main	_main/crates/ctx-history-index/Cargo.toml
EOF
ln -s "${repo}/inputs/sbom-inventory.txt" \
  "${route_runfiles}/sbom-inventory.txt"
ln -s "${repo}/inputs/license-materials.txt" \
  "${route_runfiles}/license-materials.txt"
ln -s "${repo}/Cargo.lock" "${route_runfiles}/Cargo.lock"
ln -s "${repo}/contracts/release-targets-v1.json" \
  "${route_runfiles}/release-targets-v1.json"

package() {
  RUNFILES_DIR="${runfiles}" \
  TEST_WORKSPACE="_main" \
  BUILD_WORKSPACE_DIRECTORY="${repo}" \
    "${source_root}/scripts/package-public-cli-bazel-release.sh" \
    --declared-artifact-runfile ctx_release_routes/linux-x64/artifact \
    --declared-rustc-runfile ctx_release_routes/linux-x64/rustc \
    --declared-sbom-inventory-runfile \
      ctx_release_routes/linux-x64/sbom-inventory.txt \
    --declared-license-materials-runfile \
      ctx_release_routes/linux-x64/license-materials.txt \
    --declared-cargo-lock-runfile ctx_release_routes/linux-x64/Cargo.lock \
    --declared-target-matrix-runfile \
      ctx_release_routes/linux-x64/release-targets-v1.json \
    --declared-target linux-x64 \
    --build-info "${build_info}" \
    "$@"
}

success_output="$(package --output-dir out-success)"
grep -Fq "public CLI distribution artifact: ctx-linux-x64" <<<"${success_output}"
test -x "${repo}/out-success/ctx"
test -s "${repo}/out-success/ctx.sha256"
test -s "${repo}/out-success/ctx.version"
test -s "${repo}/out-success/ctx.build-info.json"
test -s "${repo}/out-success/ctx.cdx.json"
test -s "${repo}/out-success/ctx.cdx.json.sha256"
test -s "${repo}/out-success/ctx.third-party-notices.txt"
test -s "${repo}/out-success/ctx.third-party-notices.txt.sha256"
test -s "${repo}/out-success/ctx.size.json"
test -s "${repo}/out-success/ctx.candidate.json"
test "$(sha256sum "${repo}/out-success/ctx.cdx.json" | awk '{print $1}')" \
  = "$(cat "${repo}/out-success/ctx.cdx.json.sha256")"
test "$(
  sha256sum "${repo}/out-success/ctx.third-party-notices.txt" | awk '{print $1}'
)" = "$(cat "${repo}/out-success/ctx.third-party-notices.txt.sha256")"
test "$(sha256sum "${repo}/out-success/ctx" | awk '{print $1}')" \
  = "$(cat "${repo}/out-success/ctx.sha256")"

bad_bazel_build_info="${repo}/inputs/linux-x64.bad-bazel.build-info.json"
python3 - "${build_info}" "${bad_bazel_build_info}" <<'PY'
import json
from pathlib import Path
import sys

source = Path(sys.argv[1])
destination = Path(sys.argv[2])
value = json.loads(source.read_bytes())
value["bazel"]["module_lock_sha256"] = "f" * 64
destination.write_text(
    json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
PY
accepted_build_info="${build_info}"
build_info="${bad_bazel_build_info}"
if package --output-dir out-bad-bazel-build-info \
  >"${test_root}/bad-bazel.stdout" 2>"${test_root}/bad-bazel.stderr"; then
  echo "mismatched Bazel build-info unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'does not match the exact source, version, target, toolchain' \
  "${test_root}/bad-bazel.stderr"
build_info="${accepted_build_info}"

if package --output-dir out-duplicate \
  --declared-artifact-runfile ctx_release_routes/linux-x64/artifact \
  >"${test_root}/duplicate.stdout" 2>"${test_root}/duplicate.stderr"; then
  echo "duplicate reserved artifact argument unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'duplicate reserved argument: --declared-artifact-runfile' \
  "${test_root}/duplicate.stderr"

if package --output-dir out-duplicate-rustc \
  --declared-rustc-runfile ctx_release_routes/linux-x64/rustc \
  >"${test_root}/duplicate-rustc.stdout" \
  2>"${test_root}/duplicate-rustc.stderr"; then
  echo "duplicate reserved rustc argument unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'duplicate reserved argument: --declared-rustc-runfile' \
  "${test_root}/duplicate-rustc.stderr"

if package --output-dir out-duplicate-license-materials \
  --declared-license-materials-runfile \
    ctx_release_routes/linux-x64/license-materials.txt \
  >"${test_root}/duplicate-license.stdout" \
  2>"${test_root}/duplicate-license.stderr"; then
  echo "duplicate reserved license materials argument unexpectedly passed" >&2
  exit 1
fi
grep -Fq \
  'duplicate reserved argument: --declared-license-materials-runfile' \
  "${test_root}/duplicate-license.stderr"

if package --output-dir out-caller-artifact --artifact "${artifact}" \
  >"${test_root}/caller.stdout" 2>"${test_root}/caller.stderr"; then
  echo "caller artifact override unexpectedly passed" >&2
  exit 1
fi
grep -Fq -- '--artifact is route-owned' "${test_root}/caller.stderr"

if package --output-dir out-caller-rustc --rustc "${repo}/inputs/rustc" \
  >"${test_root}/caller-rustc.stdout" 2>"${test_root}/caller-rustc.stderr"; then
  echo "caller rustc override unexpectedly passed" >&2
  exit 1
fi
grep -Fq -- '--rustc is route-owned' "${test_root}/caller-rustc.stderr"

foreign="${repo}/bazel-out/foreign/bin/crates/ctx-cli/ctx"
mkdir -p "$(dirname "${foreign}")"
sed 's/CTX_RELEASE_BUILD_TARGET=x86_64-unknown-linux-gnu/CTX_RELEASE_BUILD_TARGET=aarch64-unknown-linux-gnu/' \
  "${artifact}" >"${foreign}"
chmod 0755 "${foreign}"
ln -sfn "${foreign}" "${route_runfiles}/artifact"
if package --output-dir out-foreign \
  >"${test_root}/foreign.stdout" 2>"${test_root}/foreign.stderr"; then
  echo "foreign target artifact unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'artifact identity does not match' "${test_root}/foreign.stderr"

stale="${repo}/bazel-out/stale/bin/crates/ctx-cli/ctx"
mkdir -p "$(dirname "${stale}")"
sed "s/${source_commit}/ffffffffffffffffffffffffffffffffffffffff/" \
  "${artifact}" >"${stale}"
chmod 0755 "${stale}"
ln -sfn "${stale}" "${route_runfiles}/artifact"
if package --output-dir out-stale \
  >"${test_root}/stale.stdout" 2>"${test_root}/stale.stderr"; then
  echo "stale source artifact unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'artifact identity does not match' "${test_root}/stale.stderr"
ln -sfn "${artifact}" "${route_runfiles}/artifact"

mkdir -p "${repo}/out-hostile-symlink"
printf 'sentinel\n' >"${test_root}/sentinel"
ln -s "${test_root}/sentinel" "${repo}/out-hostile-symlink/ctx"
if package --output-dir out-hostile-symlink \
  >"${test_root}/symlink.stdout" 2>"${test_root}/symlink.stderr"; then
  echo "hostile symlink output leaf unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'already exists (symlink)' "${test_root}/symlink.stderr"
grep -Fqx 'sentinel' "${test_root}/sentinel"
test ! -e "${repo}/out-hostile-symlink/ctx.sha256"

mkdir -p "${repo}/out-hostile-directory/ctx.sha256"
if package --output-dir out-hostile-directory \
  >"${test_root}/directory.stdout" 2>"${test_root}/directory.stderr"; then
  echo "hostile directory output leaf unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'already exists (directory)' "${test_root}/directory.stderr"
test ! -e "${repo}/out-hostile-directory/ctx"

mkdir -p "${repo}/out-hostile-fifo"
mkfifo "${repo}/out-hostile-fifo/ctx.version"
if package --output-dir out-hostile-fifo \
  >"${test_root}/fifo.stdout" 2>"${test_root}/fifo.stderr"; then
  echo "hostile nonregular output leaf unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'already exists (nonregular file)' "${test_root}/fifo.stderr"
test ! -e "${repo}/out-hostile-fifo/ctx"

if CTX_FAKE_COLLISION_PATH="${repo}/out-race/ctx.sha256" \
  package --output-dir out-race \
  >"${test_root}/race.stdout" 2>"${test_root}/race.stderr"; then
  echo "racing output collision unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'already exists (regular file)' "${test_root}/race.stderr"
grep -Fqx 'hostile' "${repo}/out-race/ctx.sha256"
test ! -e "${repo}/out-race/ctx"
test ! -e "${repo}/out-race/ctx.build-info.json"

printf 'dirty\n' >>"${repo}/tracked.txt"
if package --output-dir out-dirty >/dev/null 2>&1; then
  echo "dirty source unexpectedly passed" >&2
  exit 1
fi
git -C "${repo}" restore -- tracked.txt

if CTX_FAKE_MUTATE_ARTIFACT=1 package \
  --output-dir out-artifact-drift >/dev/null 2>&1; then
  echo "post-check artifact mutation unexpectedly passed" >&2
  exit 1
fi

if CTX_FAKE_MUTATE_SOURCE=1 package \
  --output-dir out-source-drift >/dev/null 2>&1; then
  echo "source mutation during hooks unexpectedly passed" >&2
  exit 1
fi
git -C "${repo}" restore -- tracked.txt

printf 'public CLI Bazel release packaging tests: OK\n'
