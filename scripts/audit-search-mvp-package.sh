#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
# shellcheck source=scripts/ci-common.sh
source "${script_dir}/ci-common.sh"
cd "${repo_root}"

ctx_init_resource_env
ctx_ensure_rust_build_toolchain

cargo_bin="${CARGO:-cargo}"
failures=0

fail() {
  failures=$((failures + 1))
  printf 'search MVP package audit failed: %s\n' "$*" >&2
}

tracked_files() {
  if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    git ls-files --cached --others --exclude-standard | while IFS= read -r path; do
      [[ -e "${path}" ]] && printf '%s\n' "${path}"
    done
  elif command -v rg >/dev/null 2>&1; then
    rg --files
  else
    find . -type f | sed 's#^\./##'
  fi
}

grep_files() {
  local pattern="$1"
  shift

  if command -v rg >/dev/null 2>&1; then
    rg -n --glob '!target/**' --glob '!Cargo.lock' --glob '!scripts/audit-search-mvp-package.sh' --glob '!scripts/check-docs.sh' --glob '!scripts/check-buildkite-pipeline.sh' -e "${pattern}" "$@"
  else
    grep -R -n -E --exclude=Cargo.lock --exclude="$(basename "$0")" -e "${pattern}" "$@"
  fi
}

if tracked_files | grep -E '^apps/ctx-dashboard(/|$)' >/dev/null; then
  fail 'tracked dashboard app files are present under apps/ctx-dashboard'
fi

if tracked_files | grep -E '^apps/.*dashboard.*/dist/|^apps/.*dashboard.*/src/assets/' >/dev/null; then
  fail 'tracked dashboard dist or source asset bundle is present'
fi

if [[ -d apps/ctx-dashboard ]]; then
  fail 'dashboard app directory exists in the checkout'
fi

if tracked_files | grep -E '^crates/work-record-(publish|report|vcs)(/|$)' >/dev/null; then
  fail 'legacy publish/report/vcs crates are present in the package-visible source tree'
fi

if tracked_files | grep -E '^(\.ctx/exec-plans|docs/exec-plans|.*exec[_-]plan.*\.md$)' >/dev/null; then
  fail 'execution plans are present in package-visible source'
fi

if tracked_files | grep -E '^(examples|assets)/' | grep -E -i 'dashboard|work-record|ctx-records|capture-spool|evidence|link-pr|publish|shim' >/dev/null; then
  fail 'tracked examples or assets contain removed product-surface material'
fi

if grep_files 'Work Recorder|work recorder|ctx publish|ctx evidence|ctx pr|ctx link-pr|ctx context|ctx update|update checks|auto-update|update-state|auto_update|CTX_UPDATE|release manifest|dashboard export|gh CLI|GhCli|upsert_github|write-shim-command|write_shim_command|capture_shim_command|shim_command_envelope' \
  README.md SECURITY.md docs skills scripts crates/ctx-cli/src >/dev/null 2>&1; then
  fail 'public docs/help/release path contains removed Work Recorder, updater, dashboard, shim, PR, or gh surface text'
fi

if grep_files 'work-record-(publish|report|vcs)[[:space:]]*=' \
  Cargo.toml \
  crates/ctx-cli/Cargo.toml \
  crates/work-record-capture/Cargo.toml \
  crates/work-record-core/Cargo.toml \
  crates/work-record-search/Cargo.toml \
  crates/work-record-store/Cargo.toml >/dev/null 2>&1; then
  fail 'default crate manifests depend on publish/report/vcs crates'
fi

cargo_tree_output="$("${cargo_bin}" tree -p ctx --edges normal 2>&1)" || {
  fail "cargo tree failed for default ctx dependency graph: ${cargo_tree_output}"
  cargo_tree_output=""
}
if printf '%s\n' "${cargo_tree_output}" | grep -E 'work-record-(publish|report|vcs)' >/dev/null; then
  fail 'default ctx dependency graph includes publish/report/vcs crates'
fi

if grep_files 'ctx dashboard|ctx shim|ctx publish|ctx evidence|ctx pr|ctx link-pr|ctx context|ctx update|ctx watch|publish pr-comment|dashboard export|gh CLI|GhCli|upsert_github|wrapper scripts|write-shim-command|write_shim_command|capture_shim_command|shim_command_envelope|ShimCommandOptions|CommandRoot::Context|CommandRoot::Update|CommandRoot::Watch|ContextArgs|UpdateArgs|WatchArgs|run_context|run_watch|maybe_auto_update|check_or_apply_update|watch_strategy|polling_catch_up' \
  Cargo.toml BUILD.bazel MODULE.bazel scripts crates/ctx-cli/src crates/work-record-capture/src crates/work-record-search/src >/dev/null 2>&1; then
  fail 'default binary/release path contains dashboard, shim, PR publish, watch, or gh integration text'
fi

if [[ "${CTX_AUDIT_SKIP_RELEASE_BUILD:-0}" != "1" ]]; then
  cargo_locked_args=()
  if [[ "${CTX_CARGO_LOCKED:-1}" != "0" && -f Cargo.lock ]]; then
    cargo_locked_args+=(--locked)
  fi
  "${cargo_bin}" build -p ctx --bin ctx --release "${cargo_locked_args[@]}"

  suffix=""
  case "$(uname -s 2>/dev/null || true)" in
    MINGW*|MSYS*|CYGWIN*) suffix=".exe" ;;
  esac
  binary="target/release/ctx${suffix}"
  if [[ ! -f "${binary}" ]]; then
    fail "release binary missing: ${binary}"
  elif command -v strings >/dev/null 2>&1; then
    binary_strings="$(strings "${binary}")"
    if printf '%s\n' "${binary_strings}" \
      | grep -E 'ctx dashboard|ctx shim|ctx publish|ctx evidence|ctx pr|ctx link-pr|ctx context|ctx update|ctx watch|GhCli|upsert_github|write-shim-command|write_shim_command|capture_shim_command|shim_command_envelope|dashboard export|maybe_auto_update|check_or_apply_update|watch_strategy|polling_catch_up' >/dev/null; then
      fail 'release ctx binary contains removed dashboard/shim/PR-publish/watch command strings'
    fi
    if printf '%s\n' "${binary_strings}" \
      | grep -E -i 'dashboard|hosted|pull_request|published_to|evidence' >/dev/null; then
      fail 'release ctx binary contains removed dashboard/hosted/PR/evidence strings'
    fi
  fi
fi

if (( failures > 0 )); then
  exit 1
fi

printf 'search MVP package audit ok\n'
