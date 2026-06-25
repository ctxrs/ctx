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
    grep -R -n -E \
      --exclude=Cargo.lock \
      --exclude="$(basename "$0")" \
      --exclude=check-docs.sh \
      --exclude=check-buildkite-pipeline.sh \
      -e "${pattern}" "$@"
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

if grep_files 'Work Recorder|work recorder|ctx publish|ctx evidence|ctx pr|ctx link-pr|ctx context|ctx update|ctx uninstall|update checks|auto-update|update-state|auto_update|CTX_UPDATE|release manifest|dashboard export|gh CLI|GhCli|upsert_github|write-shim-command|write_shim_command|capture_shim_command|shim_command_envelope' \
  .bazelignore .bazelrc .bazelversion .buildkite .gitignore README.md SECURITY.md docs skills scripts crates/ctx-cli/src >/dev/null 2>&1; then
  fail 'public docs/help/release path contains removed Work Recorder, updater, dashboard, shim, PR, or gh surface text'
fi

if grep_files 'work-record-(publish|report|vcs)[[:space:]]*=' \
  Cargo.toml \
  crates/ctx-cli/Cargo.toml \
  crates/ctx-history-capture/Cargo.toml \
  crates/ctx-history-core/Cargo.toml \
  crates/ctx-history-search/Cargo.toml \
  crates/ctx-history-store/Cargo.toml >/dev/null 2>&1; then
  fail 'default crate manifests depend on publish/report/vcs crates'
fi

cargo_tree_output="$("${cargo_bin}" tree -p ctx --edges normal 2>&1)" || {
  fail "cargo tree failed for default ctx dependency graph: ${cargo_tree_output}"
  cargo_tree_output=""
}
if printf '%s\n' "${cargo_tree_output}" | grep -E 'work-record-(publish|report|vcs)' >/dev/null; then
  fail 'default ctx dependency graph includes publish/report/vcs crates'
fi

cargo_metadata_output="$("${cargo_bin}" metadata --no-deps --format-version 1 2>&1)" || {
  fail "cargo metadata failed for default ctx package graph: ${cargo_metadata_output}"
  cargo_metadata_output=""
}
if [[ -n "${cargo_metadata_output}" ]]; then
  if command -v jq >/dev/null 2>&1; then
    ctx_bin_targets="$(
      printf '%s\n' "${cargo_metadata_output}" \
        | jq -r '.packages[] | select(.name == "ctx") | .targets[] | select(.kind | index("bin")) | .name'
    )"
  elif command -v python3 >/dev/null 2>&1; then
    ctx_bin_targets="$(
      printf '%s\n' "${cargo_metadata_output}" \
        | python3 -c 'import json, sys
metadata = json.load(sys.stdin)
for package in metadata.get("packages", []):
    if package.get("name") != "ctx":
        continue
    for target in package.get("targets", []):
        if "bin" in target.get("kind", []):
            print(target.get("name", ""))
'
    )"
  else
    ctx_bin_targets="$(
      printf '%s\n' "${cargo_metadata_output}" \
        | grep -o '"kind":\["bin"\][^}]*"name":"[^"]*"' \
        | sed -E 's/.*"name":"([^"]*)".*/\1/'
    )"
  fi

  normalized_ctx_bins="$(printf '%s\n' "${ctx_bin_targets}" | sed '/^$/d' | sort | paste -sd ' ' -)"
  if [[ "${normalized_ctx_bins}" != "ctx" ]]; then
    fail "ctx package exposes unexpected binary targets: ${normalized_ctx_bins:-<none>}"
  fi
fi

if grep_files 'ctx dashboard|ctx shim|ctx publish|ctx evidence|ctx pr|ctx link-pr|ctx context|ctx update|ctx uninstall|ctx watch|publish pr-comment|dashboard export|gh CLI|GhCli|upsert_github|wrapper scripts|write-shim-command|write_shim_command|capture_shim_command|shim_command_envelope|ShimCommandOptions|CommandRoot::Context|CommandRoot::Update|CommandRoot::Uninstall|CommandRoot::Watch|ContextArgs|UpdateArgs|UninstallArgs|WatchArgs|run_context|run_update|run_uninstall|run_watch|maybe_auto_update|check_or_apply_update|watch_strategy|polling_catch_up' \
  .bazelignore .bazelrc .bazelversion .buildkite .gitignore Cargo.toml BUILD.bazel MODULE.bazel scripts crates/ctx-cli/src crates/ctx-history-capture/src crates/ctx-history-search/src >/dev/null 2>&1; then
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
      | grep -E 'ctx dashboard|ctx shim|ctx publish|ctx evidence|ctx pr|ctx link-pr|ctx context|ctx update|ctx uninstall|ctx watch|GhCli|upsert_github|write-shim-command|write_shim_command|capture_shim_command|shim_command_envelope|dashboard export|maybe_auto_update|check_or_apply_update|run_update|run_uninstall|watch_strategy|polling_catch_up' >/dev/null; then
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
