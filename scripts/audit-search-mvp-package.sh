#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
cd "${repo_root}"
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
    rg -n --glob '!target/**' --glob '!**/__pycache__/**' --glob '!*.pyc' --glob '!Cargo.lock' --glob '!scripts/audit-search-mvp-package.sh' --glob '!scripts/check-release-source-surface.sh' --glob '!scripts/tests/fixtures/release-source-surface/**' --glob '!scripts/check-docs.sh' --glob '!scripts/check-buildkite-pipeline.sh' -e "${pattern}" "$@"
  else
    grep -R -n -E \
      --exclude='*.pyc' \
      --exclude-dir=__pycache__ \
      --exclude=Cargo.lock \
      --exclude="$(basename "$0")" \
      --exclude=check-release-source-surface.sh \
      --exclude-dir=release-source-surface \
      --exclude=check-docs.sh \
      --exclude=check-buildkite-pipeline.sh \
      -e "${pattern}" "$@"
  fi
}

public_user_docs=(
  README.md
  SECURITY.md
  docs/*.md
  docs/contracts/*.md
  skills/ctx-agent-history-search/SKILL.md
  plugins/ctx-agent-history-search/skills/ctx-agent-history-search/SKILL.md
)

if tracked_files | grep -E '^apps/ctx-dashboard(/|$)' >/dev/null; then
  fail 'tracked dashboard app files are present under apps/ctx-dashboard'
fi

if tracked_files | grep -E '^apps/.*dashboard.*/dist/|^apps/.*dashboard.*/src/assets/' >/dev/null; then
  fail 'tracked dashboard dist or source asset bundle is present'
fi

if [[ -d apps/ctx-dashboard ]]; then
  fail 'dashboard app directory exists in the checkout'
fi

if tracked_files | grep -E '^crates/work-[r]ecord-(publish|report|vcs)(/|$)' >/dev/null; then
  fail 'legacy publish/report/vcs crates are present in the package-visible source tree'
fi

if tracked_files | grep -E '^(\.ctx/exec-plans|docs/exec-plans|.*exec[_-]plan.*\.md$)' >/dev/null; then
  fail 'execution plans are present in package-visible source'
fi

if tracked_files | grep -E '^(examples|assets)/' | grep -E -i 'dashboard|work-[r]ecord|ctx-records|capture-spool|evidence|link-pr|publish|shim|provider-live|completion-certificate|freebsd-native-release-proof|r2-' >/dev/null; then
  fail 'tracked examples or assets contain removed product-surface material'
fi

if grep_files 'dashboard|shim|shims|pull request|pull-request|pr evidence|pr-evidence|ctx publish|ctx evidence|ctx pr([^[:alnum:]_]|$)|ctx link-pr|ctx context|ctx update|ctx uninstall|\bADE\b|[Aa]mpcode|normalized-only|normalized only|normalized_import_only|normalized provider JSONL|CTX_PROVIDER_NORMALIZED_IMPORT_DEV|provider-live|completion-certificate|freebsd-native-release-proof|r2-|[W]ork Recorder|[w]ork recorder|\bwork-[r]ecord\b' \
  "${public_user_docs[@]}" >/dev/null 2>&1; then
  fail 'public docs contain removed product-surface wording'
fi

if grep_files '/home/[d]addy|/home/[^[:space:]]+/(code|Documents|Desktop)|/Users/[^[:space:]]+/(code|Documents|Desktop)|ctx-[p]rivate|ctx-multi-repo-workspace|\.ctx/worktrees' \
  .bazelignore .bazelrc .bazelversion .buildkite .gitignore README.md SECURITY.md docs skills plugins scripts crates/ctx-cli/src >/dev/null 2>&1; then
  fail 'public package surface contains private host or workspace paths'
fi

if ! diff -u skills/ctx-agent-history-search/SKILL.md plugins/ctx-agent-history-search/skills/ctx-agent-history-search/SKILL.md >/dev/null; then
  fail 'plugin skill copy differs from public skill source'
fi

if grep_files '[W]ork Recorder|[w]ork recorder|ctx publish|ctx evidence|ctx pr([^[:alnum:]_]|$)|ctx link-pr|ctx context|ctx update|ctx uninstall|update checks|auto-update|update-state|auto_update|CTX_UPDATE|provider-live|completion-certificate|freebsd-native-release-proof|r2-|dashboard export|gh CLI|GhCli|upsert_github|write-shim-command|write_shim_command|capture_shim_command|shim_command_envelope|\bADE\b|[Aa]mpcode' \
  .bazelignore .bazelrc .bazelversion .buildkite .gitignore README.md SECURITY.md docs skills scripts crates/ctx-cli/src >/dev/null 2>&1; then
  fail 'public docs/help/release path contains removed product-surface text'
fi

if grep_files 'work-[r]ecord-(publish|report|vcs)[[:space:]]*=' \
  Cargo.toml \
  crates/ctx-cli/Cargo.toml \
  crates/ctx-history-capture/Cargo.toml \
  crates/ctx-history-core/Cargo.toml \
  crates/ctx-history-index/Cargo.toml \
  crates/ctx-history-search/Cargo.toml \
  crates/ctx-history-relational/Cargo.toml >/dev/null 2>&1; then
  fail 'default crate manifests depend on publish/report/vcs crates'
fi

if ! grep -Fxq \
  'tantivy = { version = "0.26.1", default-features = false, features = ["mmap", "lz4-compression", "columnar-zstd-compression"] }' \
  Cargo.toml; then
  fail 'workspace Tantivy dependency must keep the exact 0.26.1 release feature contract'
fi

if ! grep -Fxq 'tantivy.workspace = true' \
  crates/ctx-history-index/Cargo.toml; then
  fail 'ctx-history-index must consume the workspace Tantivy release contract'
fi

if ! grep -A2 -Fx 'name = "tantivy"' Cargo.lock \
  | grep -Fxq 'version = "0.26.1"'; then
  fail 'Cargo.lock must select Tantivy 0.26.1'
fi

if grep -Fxq 'name = "rust-stemmers"' Cargo.lock; then
  fail 'Cargo.lock contains the disabled Tantivy stemming dependency'
fi

if ! bash scripts/check-release-source-surface.sh "${repo_root}"; then
  fail 'default binary/release path contains dashboard, shim, PR publish, watch, or gh integration text'
fi

if [[ "${CTX_AUDIT_SKIP_RELEASE_BUILD:-0}" != "1" ]]; then
  binary="${CTX_AUDIT_CTX_BINARY:-}"
  if [[ -z "${binary}" || ! -f "${binary}" ]]; then
    fail "native Bazel ctx binary missing: ${binary:-<unset>}"
  elif command -v strings >/dev/null 2>&1; then
    binary_strings="$(strings "${binary}")"
    if printf '%s\n' "${binary_strings}" \
      | grep -E 'ctx (dashboard|shim|publish|evidence|link-pr|context|update|uninstall|watch)([^[:alnum:]_-]|$)|ctx pr([^[:alnum:]_-]|$)|GhCli|upsert_github|write-shim-command|write_shim_command|capture_shim_command|shim_command_envelope|dashboard export|maybe_auto_update|check_or_apply_update|(^|[^[:alnum:]_])run_update([^[:alnum:]_]|$)|watch_strategy|polling_catch_up' >/dev/null; then
      fail 'release ctx binary contains removed dashboard/shim/PR-publish/watch command strings'
    fi
    if ! printf '%s\n' "${binary_strings}" \
      | bash scripts/check-release-binary-strings.sh; then
      fail 'release ctx binary contains removed hosted-history runtime strings'
    fi
  fi
fi

if (( failures > 0 )); then
  exit 1
fi

printf 'search MVP package audit ok\n'
