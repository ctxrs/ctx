#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 ]]; then
  echo "usage: package-action.sh PNPM PACKAGE_DIR SCRIPT [ARGS...]" >&2
  exit 64
fi

pnpm_bin="$1"
readonly package_dir="$2"
readonly package_script="$3"
shift 3

if [[ "$package_dir" = /* || "$package_dir" == *".."* ]]; then
  echo "error: package directory is outside the declared workspace: $package_dir" >&2
  exit 64
fi

if [[ "$pnpm_bin" != /* ]]; then
  pnpm_bin="$(cd "$(dirname "$pnpm_bin")" && pwd -P)/$(basename "$pnpm_bin")"
fi
readonly pnpm_bin

repo_root=""
runfiles_repo=false
select_repo_root() {
  local candidate="$1"
  if [[ -n "$candidate" && -f "$candidate/$package_dir/package.json" ]]; then
    repo_root="$(cd "$candidate" && pwd -P)"
    return 0
  fi
  return 1
}

if [[ -n "${RUNFILES_DIR:-}" ]] \
  && select_repo_root "$RUNFILES_DIR/${TEST_WORKSPACE:-_main}"; then
  runfiles_repo=true
elif [[ -f "$PWD/../_repo_mapping" || -f "$PWD/../MANIFEST" ]] \
  && select_repo_root "$PWD"; then
  runfiles_repo=true
elif select_repo_root "${BUILD_WORKSPACE_DIRECTORY:-}"; then
  :
elif select_repo_root "$(dirname "${BASH_SOURCE[0]}")/../.."; then
  :
else
  echo "error: package directory is outside the declared workspace: $package_dir" >&2
  exit 64
fi
if [[ ! -x "$pnpm_bin" ]]; then
  echo "error: declared pnpm executable is unavailable for package-owned action $package_dir:$package_script: $pnpm_bin" >&2
  exit 69
fi

# rules_js launchers require BAZEL_BINDIR even when a declared js_binary tool is
# invoked from a shell command router rather than a js_run_binary build action.
export BAZEL_BINDIR="${BAZEL_BINDIR:-.}"
# Package scripts commonly invoke `pnpm` recursively. Keep those calls bound to
# the same declared executable instead of falling back to an ambient install.
export PATH="$(dirname "$pnpm_bin"):$PATH"

materialized_root=""
cleanup_materialized_root() {
  if [[ -z "$materialized_root" ]]; then
    return
  fi
  case "$materialized_root" in
    "$package_tmp_base"/ctx-package-action.*)
      rm -rf -- "$materialized_root"
      ;;
    *)
      echo "error: refusing to clean unexpected package-action path: $materialized_root" >&2
      return 1
      ;;
  esac
}

materialize_runfiles_repo() {
  local source_root="$1"
  local destination_root="$2"
  local source relative destination
  while IFS= read -r -d '' source; do
    relative="${source#"$source_root"/}"
    destination="$destination_root/$relative"
    mkdir -p "$(dirname "$destination")"
    if [[ -L "$source" && -d "$source" ]]; then
      cp -RL -- "$source" "$destination"
    else
      cp -Lp -- "$source" "$destination"
    fi
  done < <(
    find "$source_root" -mindepth 1 \
      \( -path "$source_root/external" \
         -o -type d -name node_modules \
         -o -type d -name .wrangler \) -prune \
      -o \( -type f -o -type l \) -print0
  )
}

if [[ "$runfiles_repo" == true ]]; then
  package_tmp_base="${TEST_TMPDIR:-${TMPDIR:-/tmp}}"
  if [[ ! -d "$package_tmp_base" || ! -w "$package_tmp_base" ]]; then
    echo "error: package-action temporary directory is unavailable: $package_tmp_base" >&2
    exit 73
  fi
  materialized_root="$(mktemp -d "$package_tmp_base/ctx-package-action.XXXXXX")"
  trap cleanup_materialized_root EXIT
  materialize_runfiles_repo "$repo_root" "$materialized_root"
  # Link the declared immutable packages, leaving node_modules itself writable
  # for task-local runner caches. Never copy or reinstall the package closure.
  if [[ -d "$repo_root/$package_dir/node_modules" ]]; then
    link_declared_dependency() {
      if [[ -L "$1" ]]; then
        cp -P -- "$1" "$2"
      else
        ln -s -- "$1" "$2"
      fi
    }
    mkdir -p "$materialized_root/$package_dir/node_modules"
    while IFS= read -r -d '' dependency; do
      dependency_name="$(basename "$dependency")"
      destination="$materialized_root/$package_dir/node_modules/$dependency_name"
      # Preserve relative store links so rules_js guarded realpath retains the
      # package's store location and can resolve its sibling dependencies.
      if [[ "$dependency_name" == @* && -d "$dependency" && ! -L "$dependency" ]]; then
        mkdir -p "$destination"
        while IFS= read -r -d '' scoped_dependency; do
          link_declared_dependency "$scoped_dependency" "$destination/$(basename "$scoped_dependency")"
        done < <(find "$dependency" -mindepth 1 -maxdepth 1 -print0)
      else
        link_declared_dependency "$dependency" "$destination"
      fi
    done < <(find "$repo_root/$package_dir/node_modules" -mindepth 1 -maxdepth 1 -print0)
  fi
  repo_root="$materialized_root"
  if [[ ! -f "$repo_root/$package_dir/package.json" ]]; then
    echo "error: materialized package omitted its declared package.json: $package_dir" >&2
    exit 66
  fi
  export BUILD_WORKSPACE_DIRECTORY="$repo_root"
fi
readonly repo_root

cd "$repo_root/$package_dir"
if [[ ! -d node_modules ]]; then
  if [[ -n "${TEST_TARGET:-}" ]]; then
    echo "error: routine package test requires declared node_modules inputs: $package_dir" >&2
    exit 69
  fi
  "$pnpm_bin" install --frozen-lockfile --ignore-scripts --prefer-offline
fi
if [[ $# -eq 0 ]]; then
  "$pnpm_bin" run "$package_script"
else
  "$pnpm_bin" run "$package_script" "$@"
fi
