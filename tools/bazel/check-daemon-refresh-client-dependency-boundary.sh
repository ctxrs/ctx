#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-daemon-refresh-client-dependency-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-daemon-refresh-client-boundary.XXXXXX")"
trap 'rm -rf -- "${tmp}"' EXIT
mkdir -p "${tmp}/home"

query() {
  env -u BUILD_WORKSPACE_DIRECTORY \
    HOME="${tmp}/home" \
    BAZEL_OUTPUT_USER_ROOT="${tmp}/bazel-output" \
    CTX_BAZEL_SANDBOX_BASE="${tmp}/bazel-sandboxes" \
    CTX_BAZEL_WORKSPACE="${repo_root}" \
    "${repo_root}/scripts/bazelw" query "$1" --output=label
}

expected_direct="${tmp}/expected-direct.txt"
printf '%s\n' \
  '//crates/ctx-daemon-refresh-client:lib' \
  '//crates/ctx-history-capture:lib' \
  '//crates/ctx-history-index:lib' \
  '//crates/ctx-history-refresh:lib' >"${expected_direct}"
query 'kind("rust_library rule", deps(//crates/ctx-daemon-refresh-client:lib, 1)) intersect //crates/...' \
  | LC_ALL=C sort -u >"${tmp}/actual-direct.txt"
if ! diff -u "${expected_direct}" "${tmp}/actual-direct.txt"; then
  echo 'unexpected direct internal dependency set for ctx-daemon-refresh-client' >&2
  exit 1
fi

expected_test_support="${tmp}/expected-test-support.txt"
printf '%s\n' \
  '//crates/ctx-daemon-refresh-client:test_support_lib' \
  '//crates/ctx-history-capture:lib' \
  '//crates/ctx-history-core:lib' \
  '//crates/ctx-history-index:lib' \
  '//crates/ctx-history-refresh:test_support_lib' >"${expected_test_support}"
query 'kind("rust_library rule", deps(//crates/ctx-daemon-refresh-client:test_support_lib, 1)) intersect //crates/...' \
  | LC_ALL=C sort -u >"${tmp}/actual-test-support.txt"
if ! diff -u "${expected_test_support}" "${tmp}/actual-test-support.txt"; then
  echo 'unexpected test-support dependency set for ctx-daemon-refresh-client' >&2
  exit 1
fi

if query 'somepath(//crates/ctx-daemon-refresh-client:lib, //crates/ctx-daemon-service:lib)' \
  | grep -q .; then
  echo 'ctx-daemon-refresh-client must not depend on ctx-daemon-service' >&2
  exit 1
fi

python3 - "${repo_root}" <<'PY'
import pathlib
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
manifest_path = root / "crates/ctx-daemon-refresh-client/Cargo.toml"
manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))

if set(manifest.get("dependencies", {})) != {
    "anyhow",
    "ctx-history-capture",
    "ctx-history-index",
    "ctx-history-refresh",
    "serde_json",
    "uuid",
}:
    raise SystemExit("ctx-daemon-refresh-client dependency inventory differs")
if set(manifest.get("dev-dependencies", {})) != {
    "ctx-history-core",
    "ctx-history-refresh",
    "tempfile",
}:
    raise SystemExit("ctx-daemon-refresh-client dev dependency inventory differs")
if manifest.get("features") != {"test-support": []}:
    raise SystemExit("ctx-daemon-refresh-client feature inventory differs")

for forbidden in ("ctx-daemon-service", "ctx-daemon-application", "ctx-daemon-cli", "ctx-cli"):
    if forbidden in manifest.get("dependencies", {}):
        raise SystemExit(f"ctx-daemon-refresh-client depends upward on {forbidden}")

reverse = []
for candidate in sorted((root / "crates").glob("*/Cargo.toml")):
    if candidate != manifest_path and "ctx-daemon-refresh-client" in candidate.read_text(encoding="utf-8"):
        reverse.append(candidate.relative_to(root).as_posix())
if reverse != ["crates/ctx-daemon-service/Cargo.toml"]:
    raise SystemExit(f"unexpected reverse Cargo consumer of ctx-daemon-refresh-client: {reverse}")
PY

printf 'ctx-daemon-refresh-client lower dependency boundary ok\n'
