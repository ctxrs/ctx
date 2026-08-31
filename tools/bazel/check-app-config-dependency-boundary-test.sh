#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo 'usage: check-app-config-dependency-boundary-test.sh CHECKER ROOT_BUILD' >&2
  exit 64
fi

checker="$(readlink -f "$1")"
root_build="$(readlink -f "$2")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-app-config-boundary-test.XXXXXX")"
trap 'rm -rf -- "${tmp}"' EXIT
fixture="${tmp}/fixture"
mkdir -p "${fixture}"/{scripts,crates/ctx-app-config/src,crates/ctx-history-core/src}
: >"${fixture}/BUILD.bazel"
: >"${fixture}/crates/ctx-app-config/Cargo.toml"
printf '%s\n' 'use ctx_history_core::{parse_capture_provider_name, CaptureProvider};' \
  >"${fixture}/crates/ctx-app-config/src/lib.rs"
printf '%s\n' 'pub fn parse_capture_provider_name(_: &str) -> Option<()> { None }' \
  >"${fixture}/crates/ctx-history-core/src/source.rs"

cat >"${fixture}/scripts/bazelw" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$2" in
  *'deps(//crates/ctx-app-config:lib, 1)'*) target=lib ;;
  *'deps(//crates/ctx-app-config:test_support_lib, 1)'*) target=test_support_lib ;;
  *'deps(//crates/ctx-app-config:lib) intersect set('* )
    [[ -f "$(dirname "$0")/forbidden" ]] && echo '//crates/ctx-semantic-index:lib'
    exit 0 ;;
  somepath\(//crates/ctx-*:*,\ //crates/ctx-app-config:lib\))
    echo '//crates/ctx-app-config:lib'
    exit 0 ;;
  *) echo "unexpected fake query: $2" >&2; exit 2 ;;
esac
printf '%s\n' \
  "//crates/ctx-app-config:${target}" \
  '//crates/ctx-history-capture:lib' \
  '//crates/ctx-history-core:lib' \
  '//crates/ctx-history-platform:lib' \
  "//crates/ctx-semantic-model:${target/test_support_lib/test_support_lib}"
SH
chmod +x "${fixture}/scripts/bazelw"

run_checker() {
  "${checker}" "${fixture}/BUILD.bazel" >"${tmp}/stdout" 2>"${tmp}/stderr"
}
expect_rejected() {
  if run_checker || ! grep -Fq "$1" "${tmp}/stderr"; then
    cat "${tmp}/stderr" >&2
    echo "boundary mutation was not rejected with: $1" >&2
    exit 1
  fi
}

"${checker}" "${root_build}"
run_checker

printf '%s\n' 'ctx-daemon-cli = { path = "../ctx-daemon-cli" }' \
  >>"${fixture}/crates/ctx-app-config/Cargo.toml"
expect_rejected 'upward product, application, runtime, or presentation dependency'
sed -i '$d' "${fixture}/crates/ctx-app-config/Cargo.toml"

printf '%s\n' 'use ctx_daemon_cli::DaemonStatus;' \
  >>"${fixture}/crates/ctx-app-config/src/lib.rs"
expect_rejected 'upward product, application, runtime, or presentation authority'
sed -i '$d' "${fixture}/crates/ctx-app-config/src/lib.rs"

: >"${fixture}/scripts/forbidden"
expect_rejected 'forbidden upward Bazel dependency path'

printf 'app-config dependency boundary mutations rejected\n'
