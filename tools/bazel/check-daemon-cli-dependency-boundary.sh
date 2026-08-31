#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-daemon-cli-dependency-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-daemon-cli-boundary.XXXXXX")"
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

assert_labels() {
  local description="$1"
  local expression="$2"
  shift 2
  printf '%s\n' "$@" | LC_ALL=C sort -u >"${tmp}/expected.txt"
  query "${expression}" | LC_ALL=C sort -u >"${tmp}/actual.txt"
  if ! diff -u "${tmp}/expected.txt" "${tmp}/actual.txt"; then
    echo "unexpected ${description} for ctx-daemon-cli" >&2
    exit 1
  fi
}

assert_labels 'direct dependency set' \
  'kind("rust_library rule", deps(//crates/ctx-daemon-cli:lib, 1)) intersect //crates/...' \
  '//crates/ctx-app-config:lib' \
  '//crates/ctx-client-observability:lib' \
  '//crates/ctx-daemon-application:lib' \
  '//crates/ctx-daemon-cli:lib' \
  '//crates/ctx-daemon-runtime:lib' \
  '//crates/ctx-daemon-service:lib' \
  '//crates/ctx-history-capture:lib' \
  '//crates/ctx-history-core:lib' \
  '//crates/ctx-history-platform:lib' \
  '//crates/ctx-history-index:lib' \
  '//crates/ctx-history-read-application:lib' \
  '//crates/ctx-history-refresh:lib' \
  '//crates/ctx-semantic-index:lib' \
  '//crates/ctx-semantic-model:lib' \
  '//crates/ctx-terminal:lib' \
  '//crates/ctx-upgrade-engine:lib'

assert_labels 'qualification dependency set' \
  'kind("rust_library rule", deps(//crates/ctx-daemon-cli:qualification_lib, 1)) intersect //crates/...' \
  '//crates/ctx-app-config:lib' \
  '//crates/ctx-client-observability:lib' \
  '//crates/ctx-daemon-application:qualification_lib' \
  '//crates/ctx-daemon-cli:qualification_lib' \
  '//crates/ctx-daemon-runtime:qualification_lib' \
  '//crates/ctx-daemon-service:qualification_lib' \
  '//crates/ctx-history-capture:lib' \
  '//crates/ctx-history-core:lib' \
  '//crates/ctx-history-platform:lib' \
  '//crates/ctx-history-index:lib' \
  '//crates/ctx-history-read-application:lib' \
  '//crates/ctx-history-refresh:lib' \
  '//crates/ctx-semantic-index:lib' \
  '//crates/ctx-semantic-model:lib' \
  '//crates/ctx-terminal:lib' \
  '//crates/ctx-upgrade-engine:qualification_lib'

assert_labels 'production reverse dependency set' \
  'attr("testonly", "0", kind("rust_binary rule", rdeps(//crates/..., //crates/ctx-daemon-cli:lib)) union kind("rust_library rule", rdeps(//crates/..., //crates/ctx-daemon-cli:lib)))' \
  '//crates/ctx-cli:ctx' \
  '//crates/ctx-cli-presentation:lib' \
  '//crates/ctx-daemon-cli:lib' \
  '//crates/ctx-history-cli:lib'

assert_labels 'test-support reverse dependency set' \
  'kind("rust_library rule", rdeps(//crates/..., //crates/ctx-daemon-cli:test_support_lib)) union kind("rust_test rule", rdeps(//crates/..., //crates/ctx-daemon-cli:test_support_lib))' \
  '//crates/ctx-cli:unit_tests' \
  '//crates/ctx-cli-presentation:test_support_lib' \
  '//crates/ctx-cli-presentation:unit_tests' \
  '//crates/ctx-daemon-cli:test_support_lib' \
  '//crates/ctx-history-cli:test_support_lib'

assert_labels 'qualification reverse dependency set' \
  'kind("rust_binary rule", rdeps(//crates/..., //crates/ctx-daemon-cli:qualification_lib)) union kind("rust_library rule", rdeps(//crates/..., //crates/ctx-daemon-cli:qualification_lib))' \
  '//crates/ctx-cli:ctx_auto_upgrade_acceptance_fixture' \
  '//crates/ctx-cli:ctx_hosted_uninstall_test_host' \
  '//crates/ctx-cli:ctx_upgrade_test_harness' \
  '//crates/ctx-daemon-cli:qualification_lib'

assert_labels 'qualification test-support reverse dependency set' \
  'kind("rust_binary rule", rdeps(//crates/..., //crates/ctx-daemon-cli:qualification_test_support_lib)) union kind("rust_library rule", rdeps(//crates/..., //crates/ctx-daemon-cli:qualification_test_support_lib))' \
  '//crates/ctx-daemon-cli:qualification_test_support_lib'

if [[ -n "$(query 'somepath(//crates/ctx-daemon-cli:lib, //crates/ctx-cli:ctx)')" ]]; then
  echo 'ctx-daemon-cli has a reverse dependency path into ctx-cli' >&2
  exit 1
fi
if [[ -z "$(query 'somepath(//crates/ctx-daemon-cli:lib, //crates/ctx-app-config:lib)')" ]]; then
  echo 'ctx-daemon-cli must consume ctx-app-config through the intended downward edge' >&2
  exit 1
fi
if [[ -n "$(query 'somepath(//crates/ctx-app-config:lib, //crates/ctx-daemon-cli:lib)')" ]]; then
  echo 'ctx-app-config must not depend upward on ctx-daemon-cli' >&2
  exit 1
fi

python3 - "${repo_root}" <<'PY'
import pathlib
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
manifest_path = root / "crates/ctx-daemon-cli/Cargo.toml"
manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
expected_dependencies = {
    "anyhow",
    "ctx-app-config",
    "ctx-client-observability",
    "ctx-daemon-application",
    "ctx-daemon-runtime",
    "ctx-daemon-service",
    "ctx-history-capture",
    "ctx-history-core",
    "ctx-history-platform",
    "ctx-history-index",
    "ctx-history-read-application",
    "ctx-history-refresh",
    "ctx-semantic-index",
    "ctx-semantic-model",
    "ctx-terminal",
    "ctx-upgrade-engine",
    "fs2",
    "serde_json",
    "thiserror",
    "uuid",
}
actual_dependencies = set(manifest.get("dependencies", {}))
if actual_dependencies != expected_dependencies:
    raise SystemExit(
        "ctx-daemon-cli dependency inventory differs: "
        f"missing={sorted(expected_dependencies - actual_dependencies)} "
        f"extra={sorted(actual_dependencies - expected_dependencies)}"
    )
expected_dev_dependencies = {
    "ctx-client-observability",
    "ctx-daemon-service",
    "ctx-history-refresh",
    "ctx-semantic-index",
    "ctx-semantic-model",
    "ctx-upgrade-engine",
    "tempfile",
    "unicode-width",
}
actual_dev_dependencies = set(manifest.get("dev-dependencies", {}))
if actual_dev_dependencies != expected_dev_dependencies:
    raise SystemExit(
        "ctx-daemon-cli dev dependency inventory differs: "
        f"missing={sorted(expected_dev_dependencies - actual_dev_dependencies)} "
        f"extra={sorted(actual_dev_dependencies - expected_dev_dependencies)}"
    )
if manifest.get("features") != {"test-support": ["ctx-daemon-service/test-support"]}:
    raise SystemExit(
        "ctx-daemon-cli features must expose only test-support forwarding to ctx-daemon-service"
    )
if manifest.get("package", {}).get("publish") is not False:
    raise SystemExit("ctx-daemon-cli must remain an internal non-published package")

reverse = []
for candidate in sorted((root / "crates").glob("*/Cargo.toml")):
    if candidate != manifest_path and "ctx-daemon-cli" in candidate.read_text(encoding="utf-8"):
        reverse.append(candidate.relative_to(root).as_posix())
if reverse != [
    "crates/ctx-cli/Cargo.toml",
    "crates/ctx-history-cli/Cargo.toml",
]:
    raise SystemExit(f"unexpected reverse Cargo consumer of ctx-daemon-cli: {reverse}")
PY

crate_root="${repo_root}/crates/ctx-daemon-cli"
find "${crate_root}/tests/contracts" -type f -name '*.rs' \
  -printf '//crates/ctx-daemon-cli:tests/contracts/%P\n' \
  | LC_ALL=C sort -u >"${tmp}/contract-sources.txt"
query 'filter("^//crates/ctx-daemon-cli:tests/contracts/", labels(srcs, kind("rust_test rule", //crates/ctx-daemon-cli:*)))' \
  | LC_ALL=C sort -u >"${tmp}/owned-contract-sources.txt"
if ! diff -u "${tmp}/contract-sources.txt" "${tmp}/owned-contract-sources.txt"; then
  echo 'ctx-daemon-cli contract sources must be live Bazel rust_test sources' >&2
  exit 1
fi
if find "${crate_root}" -type l -print -quit | grep -q .; then
  echo 'ctx-daemon-cli must contain no symlinked source or metadata' >&2
  exit 1
fi
if find "${crate_root}" -name '*.rs' ! -path "${crate_root}/src/*" ! -path "${crate_root}/tests/contracts/*" -print -quit | grep -q .; then
  echo 'ctx-daemon-cli Rust sources must remain package-local under src or its reviewed Bazel-only contracts' >&2
  exit 1
fi
if grep -En 'ctx-(agent-application|agent-integrations|cli|history-ingest-application|protocol)([^[:alnum:]_-]|$)|(^|[^[:alnum:]_-])(clap|ureq)([^[:alnum:]_-]|$)' \
  "${crate_root}/Cargo.toml"; then
  echo 'excluded product, parser, provider, or concrete network dependency leaked into ctx-daemon-cli' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'ctx_(agent_application|agent_integrations|cli|history_ingest_application|protocol)::|(^|[^[:alnum:]_])(clap|ureq)::' \
  "${crate_root}/src"; then
  echo 'upward CLI, parser, provider, or unrelated application authority leaked into ctx-daemon-cli source' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'include!|Box::leak|(^|[^[:alnum:]_])unsafe([^[:alnum:]_]|$)' \
  "${crate_root}/src"; then
  echo 'source indirection, lifetime escape, or unsafe boundary leaked into ctx-daemon-cli' >&2
  exit 1
fi

printf 'ctx-daemon-cli dependency, locality, source ownership, and bounded-consumer boundary ok\n'
