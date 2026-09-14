#!/usr/bin/env bash
set -euo pipefail

if (( "$#" > 1 )); then
  printf 'usage: %s [source-root]\n' "$(basename "$0")" >&2
  exit 2
fi

source_root="${1:-.}"
if [[ ! -d "${source_root}" ]]; then
  printf 'release source root not found: %s\n' "${source_root}" >&2
  exit 2
fi

cd "${source_root}"

# These are the package-visible release-policy inputs. BUILD.bazel carries the
# same paths into the Bazel runfiles tree, so direct and Bazel audits inspect an
# identical source surface.
source_paths=(
  .bazelignore
  .bazelrc
  .bazelversion
  .buildkite
  .gitignore
  Cargo.toml
  BUILD.bazel
  MODULE.bazel
  README.md
  SECURITY.md
  docs
  skills
  scripts
  crates/ctx-agent-application/src
  crates/ctx-agent-integrations/src
  crates/ctx-app-config/Cargo.toml
  crates/ctx-app-config/src
  crates/ctx-cli/Cargo.toml
  crates/ctx-cli/src
  crates/ctx-cli-presentation/Cargo.toml
  crates/ctx-cli-presentation/src
  crates/ctx-client-observability/src
  crates/ctx-daemon-cli/src
  crates/ctx-daemon-application/src
  crates/ctx-daemon-runtime/src
  crates/ctx-daemon-service/src
  crates/ctx-terminal/src
  crates/ctx-managed-pair-engine/src
  crates/ctx-upgrade-engine/src
  crates/ctx-history-capture/src
  crates/ctx-history-capture-composition/src
  crates/ctx-history-provider-gemini/src
  crates/ctx-history-provider-native-jsonl/src
  crates/ctx-history-provider-docproj/src
  crates/ctx-history-provider-hermes/Cargo.toml
  crates/ctx-history-provider-hermes/src
  crates/ctx-history-cli/src
  crates/ctx-history-jsonl/src
  crates/ctx-history-native-jsonl-parsers/src
  crates/ctx-history-provider-runtime/src
  crates/ctx-history-provider-claude-cursor/src
  crates/ctx-history-providers-jsonl-shared/src
  crates/ctx-history-providers-sqlite-selected/src
  crates/ctx-history-providers-sqlite-inventory/Cargo.toml
  crates/ctx-history-providers-sqlite-inventory/src
  crates/ctx-history-providers-task-docs/src
  crates/ctx-history-read-application/src
  crates/ctx-history-capture-model/src
  crates/ctx-history-capture-runtime/src
  crates/ctx-history-provider-mistral-mux/src
  crates/ctx-history-openclaw-schema/Cargo.toml
  crates/ctx-history-openclaw-schema/src
  crates/ctx-history-provider-openclaw-sqlite/Cargo.toml
  crates/ctx-history-provider-openclaw-sqlite/src
  crates/ctx-history-source-discovery/src
  crates/ctx-history-source-io/src
  crates/ctx-history-source-sqlite/src
  crates/ctx-history-search/src
)

removed_surface_pattern='ctx (dashboard|shim|publish|evidence|link-pr|context|uninstall|watch)([^[:alnum:]_-]|$)'
removed_surface_pattern+='|ctx update([[:space:]]+--|[^[:alnum:]_ -]|[[:space:]]*$)'
removed_surface_pattern+='|ctx pr([^[:alnum:]_-]|$)|publish pr-comment|dashboard export|gh CLI|GhCli|upsert_github|wrapper scripts'
removed_surface_pattern+='|write-shim-command|write_shim_command|capture_shim_command|shim_command_envelope'
removed_surface_pattern+='|(^|[^[:alnum:]_])ShimCommandOptions([^[:alnum:]_]|$)'
removed_surface_pattern+='|CommandRoot::Context([^[:alnum:]_]|$)|CommandRoot::Update([^[:alnum:]_]|$)|CommandRoot::Uninstall([^[:alnum:]_]|$)|CommandRoot::Watch([^[:alnum:]_]|$)'
removed_surface_pattern+='|(^|[^[:alnum:]_])(ContextArgs|UpdateArgs|UninstallArgs|WatchArgs)([^[:alnum:]_]|$)'
removed_surface_pattern+='|(^|[^[:alnum:]_])run_(context|update|watch)([^[:alnum:]_]|$)'
removed_surface_pattern+='|maybe_auto_update|check_or_apply_update|watch_strategy|polling_catch_up'

failures=0

check_file() {
  local path="$1"
  # Fixture names retain their production path identity for exact allowlists.
  local policy_path="${path%.fixture}"

  case "${path}" in
    scripts/audit-search-mvp-package.sh|scripts/check-release-source-surface.sh|scripts/check-docs.sh|scripts/check-buildkite-pipeline.sh|scripts/tests/fixtures/release-source-surface/*)
      return 0
      ;;
    target/*|*/target/*|*/__pycache__/*|*.pyc|Cargo.lock)
      return 0
      ;;
  esac

  case "${policy_path}" in
    crates/ctx-history-openclaw-schema/Cargo.toml|crates/ctx-history-provider-hermes/Cargo.toml|crates/ctx-history-provider-openclaw-sqlite/Cargo.toml|crates/ctx-history-providers-sqlite-inventory/Cargo.toml)
      if ! LC_ALL=C grep -q -E '^[[:space:]]*version\.workspace[[:space:]]*=[[:space:]]*true[[:space:]]*$' "${path}" \
        || LC_ALL=C grep -q -E '^[[:space:]]*version[[:space:]]*=' "${path}"; then
        printf 'release product crate must inherit the workspace version: %s\n' "${path}" >&2
        failures=$((failures + 1))
      fi
      ;;
    crates/ctx-history-capture/src/pro_output.rs|crates/ctx-history-capture/src/repository_attribution/*)
      printf 'release source contains retired capture authority: %s\n' "${path}" >&2
      failures=$((failures + 1))
      ;;
  esac

  if [[ "${policy_path}" != "crates/ctx-history-capture-model/src/exact_json.rs" ]] \
    && LC_ALL=C grep -n -E 'fn (raw_object_keys_are_unique|exact_json_value)[(]' "${path}" >/dev/null 2>&1; then
    printf 'release source defines exact JSON authority outside ctx-history-capture-model: %s\n' "${path}" >&2
    failures=$((failures + 1))
  fi

  if LC_ALL=C grep -n -E "${removed_surface_pattern}" "${path}" >/dev/null 2>&1; then
    printf 'release source contains a removed top-level/cloud surface: %s\n' "${path}" >&2
    failures=$((failures + 1))
  fi

  if LC_ALL=C grep -n -E '(^|[^[:alnum:]_])run_uninstall([^[:alnum:]_]|$)' "${path}" >/dev/null 2>&1; then
    printf 'release source contains a removed top-level uninstall implementation: %s\n' "${path}" >&2
    failures=$((failures + 1))
  fi
}

for source_path in "${source_paths[@]}"; do
  if [[ ! -e "${source_path}" && -e "${source_path}.fixture" ]]; then
    source_path="${source_path}.fixture"
  fi
  [[ -e "${source_path}" ]] || continue
  if [[ -d "${source_path}" ]]; then
    # Bazel runfiles are symlink forests. Follow those declared inputs so the
    # sandboxed audit examines the same file bytes as a direct checkout.
    while IFS= read -r path; do
      check_file "${path}"
    done < <(find -L "${source_path}" -type f -print | LC_ALL=C sort)
  else
    check_file "${source_path}"
  fi
done

if (( failures > 0 )); then
  exit 1
fi

printf 'release source surface audit ok\n'
