#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-daemon-service-dependency-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-daemon-service-boundary.XXXXXX")"
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
  '//crates/ctx-client-observability:lib' \
  '//crates/ctx-daemon-runtime:lib' \
  '//crates/ctx-daemon-service:lib' \
  '//crates/ctx-history-capture:lib' \
  '//crates/ctx-history-core:lib' \
  '//crates/ctx-history-index:lib' \
  '//crates/ctx-history-refresh:lib' \
  '//crates/ctx-semantic-index:lib' \
  '//crates/ctx-semantic-model:lib' \
  '//crates/ctx-upgrade-engine:lib' >"${expected_direct}"
query 'kind("rust_library rule", deps(//crates/ctx-daemon-service:lib, 1)) intersect //crates/...' \
  | LC_ALL=C sort -u >"${tmp}/actual-direct.txt"
if ! diff -u "${expected_direct}" "${tmp}/actual-direct.txt"; then
  echo 'unexpected direct internal dependency set for ctx-daemon-service' >&2
  exit 1
fi

expected_qualification="${tmp}/expected-qualification.txt"
printf '%s\n' \
  '//crates/ctx-client-observability:lib' \
  '//crates/ctx-daemon-runtime:qualification_lib' \
  '//crates/ctx-daemon-service:qualification_lib' \
  '//crates/ctx-history-capture:lib' \
  '//crates/ctx-history-core:lib' \
  '//crates/ctx-history-index:lib' \
  '//crates/ctx-history-refresh:lib' \
  '//crates/ctx-semantic-index:lib' \
  '//crates/ctx-semantic-model:lib' \
  '//crates/ctx-upgrade-engine:qualification_lib' >"${expected_qualification}"
query 'kind("rust_library rule", deps(//crates/ctx-daemon-service:qualification_lib, 1)) intersect //crates/...' \
  | LC_ALL=C sort -u >"${tmp}/actual-qualification.txt"
if ! diff -u "${expected_qualification}" "${tmp}/actual-qualification.txt"; then
  echo 'unexpected qualification dependency set for ctx-daemon-service' >&2
  exit 1
fi

expected_test_support="${tmp}/expected-test-support.txt"
printf '%s\n' \
  '//crates/ctx-client-observability:test_support_lib' \
  '//crates/ctx-daemon-runtime:lib' \
  '//crates/ctx-daemon-service:test_support_lib' \
  '//crates/ctx-history-capture-model:lib' \
  '//crates/ctx-history-capture:lib' \
  '//crates/ctx-history-core:lib' \
  '//crates/ctx-history-index:lib' \
  '//crates/ctx-history-refresh:test_support_lib' \
  '//crates/ctx-semantic-index:test_support_lib' \
  '//crates/ctx-semantic-model:test_support_lib' \
  '//crates/ctx-upgrade-engine:test_support_lib' >"${expected_test_support}"
query 'kind("rust_library rule", deps(//crates/ctx-daemon-service:test_support_lib, 1)) intersect //crates/...' \
  | LC_ALL=C sort -u >"${tmp}/actual-test-support.txt"
if ! diff -u "${expected_test_support}" "${tmp}/actual-test-support.txt"; then
  echo 'unexpected test-support dependency set for ctx-daemon-service' >&2
  exit 1
fi

expected_reverse_test_support="${tmp}/expected-reverse-test-support.txt"
printf '%s\n' \
  '//crates/ctx-cli-presentation:test_support_lib' \
  '//crates/ctx-cli-presentation:unit_tests' \
  '//crates/ctx-cli:unit_tests' \
  '//crates/ctx-daemon-application:test_support_lib' \
  '//crates/ctx-daemon-cli:test_support_lib' \
  '//crates/ctx-daemon-cli:unit_tests' \
  '//crates/ctx-daemon-service:test_support_lib' \
  '//crates/ctx-history-cli:test_support_lib' >"${expected_reverse_test_support}"
query 'kind("rust_library rule", rdeps(//crates/..., //crates/ctx-daemon-service:test_support_lib)) union kind("rust_test rule", rdeps(//crates/..., //crates/ctx-daemon-service:test_support_lib))' \
  | LC_ALL=C sort -u >"${tmp}/actual-reverse-test-support.txt"
if ! diff -u "${expected_reverse_test_support}" "${tmp}/actual-reverse-test-support.txt"; then
  echo 'unexpected reverse test-support consumer of ctx-daemon-service' >&2
  exit 1
fi

expected_reverse="${tmp}/expected-reverse.txt"
# The production binary uses the production service variant; qualification
# fixtures use the source-identical qualification variant.
printf '%s\n' \
  '//crates/ctx-cli-presentation:lib' \
  '//crates/ctx-cli-presentation:qualification_lib' \
  '//crates/ctx-cli:ctx' \
  '//crates/ctx-cli:ctx_auto_upgrade_acceptance_fixture' \
  '//crates/ctx-cli:ctx_hosted_uninstall_test_host' \
  '//crates/ctx-cli:ctx_upgrade_test_harness' \
  '//crates/ctx-daemon-application:lib' \
  '//crates/ctx-daemon-cli:lib' \
  '//crates/ctx-daemon-cli:qualification_test_support_lib' \
  '//crates/ctx-daemon-service:lib' \
  '//crates/ctx-history-cli:lib' >"${expected_reverse}"
query 'kind("rust_binary rule", rdeps(//crates/..., //crates/ctx-daemon-service:lib)) union kind("rust_library rule", rdeps(//crates/..., //crates/ctx-daemon-service:lib))' \
  | LC_ALL=C sort -u >"${tmp}/actual-reverse.txt"
if ! diff -u "${expected_reverse}" "${tmp}/actual-reverse.txt"; then
  echo 'ctx-daemon-service must have only the application seam and CLI as reverse production consumers' >&2
  exit 1
fi

expected_reverse_qualification="${tmp}/expected-reverse-qualification.txt"
printf '%s\n' \
  '//crates/ctx-cli:ctx_auto_upgrade_acceptance_fixture' \
  '//crates/ctx-cli:ctx_hosted_uninstall_test_host' \
  '//crates/ctx-cli:ctx_upgrade_test_harness' \
  '//crates/ctx-daemon-application:qualification_lib' \
  '//crates/ctx-daemon-cli:qualification_lib' \
  '//crates/ctx-daemon-service:qualification_lib' >"${expected_reverse_qualification}"
query 'kind("rust_binary rule", rdeps(//crates/..., //crates/ctx-daemon-service:qualification_lib)) union kind("rust_library rule", rdeps(//crates/..., //crates/ctx-daemon-service:qualification_lib))' \
  | LC_ALL=C sort -u >"${tmp}/actual-reverse-qualification.txt"
if ! diff -u "${expected_reverse_qualification}" "${tmp}/actual-reverse-qualification.txt"; then
  echo 'unexpected reverse qualification consumer of ctx-daemon-service' >&2
  exit 1
fi

python3 - "${repo_root}" <<'PY'
import pathlib
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
manifest_path = root / "crates/ctx-daemon-service/Cargo.toml"
manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
dependencies = set(manifest.get("dependencies", {}))
targets = manifest.get("target", {})
if set(targets) != {"cfg(unix)", 'cfg(target_os = "macos")'}:
    raise SystemExit("ctx-daemon-service target dependency tables differ")
for target in targets.values():
    dependencies.update(target.get("dependencies", {}))

allowed_internal = {
    "ctx-client-observability",
    "ctx-daemon-runtime",
    "ctx-history-capture",
    "ctx-history-core",
    "ctx-history-index",
    "ctx-history-refresh",
    "ctx-semantic-index",
    "ctx-semantic-model",
    "ctx-upgrade-engine",
}
allowed_external = {
    "anyhow",
    "ctrlc",
    "fs2",
    "libc",
    "objc2-foundation",
    "rusqlite",
    "serde_json",
    "uuid",
}
allowed = allowed_internal | allowed_external
if dependencies != allowed:
    raise SystemExit(
        "ctx-daemon-service dependency inventory differs: "
        f"missing={sorted(allowed - dependencies)} extra={sorted(dependencies - allowed)}"
    )
expected_dev = {
    "ctx-client-observability",
    "ctx-history-capture-model",
    "ctx-history-platform",
    "ctx-history-refresh",
    "ctx-semantic-index",
    "ctx-semantic-model",
    "ctx-upgrade-engine",
    "tempfile",
}
actual_dev = set(manifest.get("dev-dependencies", {}))
if actual_dev != expected_dev:
    raise SystemExit(
        "ctx-daemon-service dev dependency inventory differs: "
        f"missing={sorted(expected_dev - actual_dev)} extra={sorted(actual_dev - expected_dev)}"
    )
if manifest.get("features") != {"test-support": []}:
    raise SystemExit("ctx-daemon-service feature inventory differs")

reverse = []
for candidate in sorted((root / "crates").glob("*/Cargo.toml")):
    if candidate != manifest_path and "ctx-daemon-service" in candidate.read_text(encoding="utf-8"):
        reverse.append(candidate.relative_to(root).as_posix())
if reverse != [
    "crates/ctx-daemon-application/Cargo.toml",
    "crates/ctx-daemon-cli/Cargo.toml",
]:
    raise SystemExit(f"unexpected reverse Cargo consumer of ctx-daemon-service: {reverse}")
PY

service_root="${repo_root}/crates/ctx-daemon-service"
if grep -En 'ctx-(agent-integrations|cli)([^[:alnum:]_-]|$)|(^|[^[:alnum:]_-])(clap|ureq)([^[:alnum:]_-]|$)' \
  "${service_root}/Cargo.toml"; then
  echo 'CLI, agent-integration, or network authority leaked into ctx-daemon-service' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'ctx_agent_integrations::|(^|[^[:alnum:]_])(clap|ureq)::|crate::(analytics|identity|net|output|ui)::|GenerationRetentionLease' \
  "${service_root}/src"; then
  echo 'composition, presentation, network, or concrete generation authority leaked into ctx-daemon-service' >&2
  exit 1
fi
if grep -REn --include='*.rs' '\bBox\b' "${service_root}/src"; then
  echo 'ctx-daemon-service ports and scheduler must remain unboxed' >&2
  exit 1
fi

printf 'ctx-daemon-service hermetic dependency and composition boundary ok\n'
