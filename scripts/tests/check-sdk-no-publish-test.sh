#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  source_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  source_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-sdk-no-publish-test.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

fixture_root="${tmp_dir}/repo"
mkdir -p \
  "${fixture_root}/scripts" \
  "${fixture_root}/sdks/typescript" \
  "${fixture_root}/sdks/python" \
  "${fixture_root}/crates/ctx-sdk" \
  "${fixture_root}/crates/ctx-protocol" \
  "${fixture_root}/sdks/dotnet/src/Ctx.AgentHistory" \
  "${fixture_root}/.git/objects" \
  "${fixture_root}/.buildkite-cache/bazel-repository" \
  "${fixture_root}/.github/workflows" \
  "${fixture_root}/src"
cp "${source_root}/scripts/check-sdk-no-publish.sh" "${fixture_root}/scripts/"

cat >"${fixture_root}/sdks/typescript/package.json" <<'EOF'
{"private": true}
EOF
cat >"${fixture_root}/sdks/python/pyproject.toml" <<'EOF'
[tool.ctx]
publish = false
EOF
printf 'publish = false\n' >"${fixture_root}/crates/ctx-sdk/Cargo.toml"
printf 'publish = false\n' >"${fixture_root}/crates/ctx-protocol/Cargo.toml"
cat >"${fixture_root}/sdks/dotnet/src/Ctx.AgentHistory/Ctx.AgentHistory.csproj" <<'EOF'
<Project><PropertyGroup><IsPackable>false</IsPackable></PropertyGroup></Project>
EOF
printf 'harmless source\n' >"${fixture_root}/src/main.txt"
printf 'npm publish\n' >"${fixture_root}/.git/objects/ignored-history"
printf 'cargo publish\n' >"${fixture_root}/.buildkite-cache/bazel-repository/ignored-cache"
hidden_source_file="${fixture_root}/.github/workflows/release job"$'\n'"matrix.sh"
printf 'harmless hidden source\n' >"$hidden_source_file"

grep_bin="$(command -v grep)"
dirname_bin="$(command -v dirname)"
find_bin="$(command -v find)"
mktemp_bin="$(command -v mktemp)"
rm_bin="$(command -v rm)"
tools_dir="${tmp_dir}/tools"
find_audit_dir="${tmp_dir}/find-audit"
mkdir -p "$tools_dir"
mkdir -p "$find_audit_dir"
ln -s "$grep_bin" "${tools_dir}/grep"
ln -s "$dirname_bin" "${tools_dir}/dirname"
ln -s "$mktemp_bin" "${tools_dir}/mktemp"
ln -s "$rm_bin" "${tools_dir}/rm"
cat >"${tools_dir}/find" <<'EOF'
#!/bin/bash
set -euo pipefail

if [[ "${FAIL_FIND:-0}" == "1" ]]; then
  exit 2
fi

saw_prune=false
saw_git=false
saw_buildkite_cache=false
for argument in "$@"; do
  case "$argument" in
    -prune) saw_prune=true ;;
    ./.git) saw_git=true ;;
    ./.buildkite-cache) saw_buildkite_cache=true ;;
  esac
done
if [[ "$saw_prune" != true || "$saw_git" != true || "$saw_buildkite_cache" != true ]]; then
  printf 'publish guard did not request generated-tree pruning\n' >&2
  exit 91
fi

audit_file="${FIND_AUDIT_DIR:?}/find-output.${BASHPID}"
trap 'rm -f -- "$audit_file"' EXIT
set +e
"${REAL_FIND:?}" "$@" >"$audit_file"
find_status=$?
set -e
if [[ "$find_status" -ne 0 ]]; then
  exit "$find_status"
fi

while IFS= read -r -d '' path; do
  case "$path" in
    ./.git | ./.git/* | ./.buildkite-cache | ./.buildkite-cache/*)
      printf 'generated tree was enumerated instead of pruned: %q\n' "$path" >&2
      exit 92
      ;;
  esac
  printf '%s\0' "$path"
done <"$audit_file"
EOF
chmod +x "${tools_dir}/find"

baseline_output="${tmp_dir}/baseline.out"
if ! REAL_FIND="$find_bin" FIND_AUDIT_DIR="$find_audit_dir" PATH="$tools_dir" /bin/bash \
  "${fixture_root}/scripts/check-sdk-no-publish.sh" >"$baseline_output" 2>&1; then
  cat "$baseline_output" >&2
  printf 'publish guard rejected a safe fixture without rg\n' >&2
  exit 1
fi
"$grep_bin" -Fq 'SDK publish guard passed' "$baseline_output"

command_name='cargo'
printf '%s %s\n' "$command_name" publish >"${fixture_root}/src/release.sh"
mutation_output="${tmp_dir}/mutation.out"
if REAL_FIND="$find_bin" FIND_AUDIT_DIR="$find_audit_dir" PATH="$tools_dir" /bin/bash \
  "${fixture_root}/scripts/check-sdk-no-publish.sh" >"$mutation_output" 2>&1; then
  cat "$mutation_output" >&2
  printf 'publish guard accepted a live publish command without rg\n' >&2
  exit 1
fi
"$grep_bin" -Fq \
  'live SDK package-manager publish command found outside docs/policy text' \
  "$mutation_output"

printf 'harmless source\n' >"${fixture_root}/src/release.sh"
printf 'npm publish\n' >"$hidden_source_file"
hidden_mutation_output="${tmp_dir}/hidden-mutation.out"
if REAL_FIND="$find_bin" FIND_AUDIT_DIR="$find_audit_dir" PATH="$tools_dir" /bin/bash \
  "${fixture_root}/scripts/check-sdk-no-publish.sh" >"$hidden_mutation_output" 2>&1; then
  cat "$hidden_mutation_output" >&2
  printf 'publish guard accepted a live publish command in a hidden tree\n' >&2
  exit 1
fi
"$grep_bin" -Fq \
  'live SDK package-manager publish command found outside docs/policy text' \
  "$hidden_mutation_output"

printf 'harmless hidden source\n' >"$hidden_source_file"

traversal_failure_output="${tmp_dir}/traversal-failure.out"
if FAIL_FIND=1 REAL_FIND="$find_bin" FIND_AUDIT_DIR="$find_audit_dir" \
  PATH="$tools_dir" /bin/bash \
  "${fixture_root}/scripts/check-sdk-no-publish.sh" >"$traversal_failure_output" 2>&1; then
  cat "$traversal_failure_output" >&2
  printf 'publish guard accepted an incomplete filesystem traversal\n' >&2
  exit 1
fi
"$grep_bin" -Fq \
  'could not traverse every SDK publish-policy input' \
  "$traversal_failure_output"

printf 'unreadable to scanner\n' >"${fixture_root}/src/scan-error.txt"
failing_tools_dir="${tmp_dir}/failing-tools"
mkdir -p "$failing_tools_dir"
ln -s "$dirname_bin" "${failing_tools_dir}/dirname"
ln -s "${tools_dir}/find" "${failing_tools_dir}/find"
ln -s "$mktemp_bin" "${failing_tools_dir}/mktemp"
ln -s "$rm_bin" "${failing_tools_dir}/rm"
cat >"${failing_tools_dir}/grep" <<'EOF'
#!/bin/bash
for argument in "$@"; do
  if [[ "$argument" == "src/scan-error.txt" ]]; then
    exit 2
  fi
done
exec "${REAL_GREP:?}" "$@"
EOF
chmod +x "${failing_tools_dir}/grep"

failure_output="${tmp_dir}/failure.out"
if REAL_FIND="$find_bin" FIND_AUDIT_DIR="$find_audit_dir" \
  REAL_GREP="$grep_bin" PATH="$failing_tools_dir" /bin/bash \
  "${fixture_root}/scripts/check-sdk-no-publish.sh" >"$failure_output" 2>&1; then
  cat "$failure_output" >&2
  printf 'publish guard accepted an incomplete policy scan\n' >&2
  exit 1
fi
"$grep_bin" -Fq 'could not inspect every SDK publish-policy input' "$failure_output"

printf 'SDK no-publish policy tests: OK (pruned generated trees, NUL-safe hidden paths, traversal/scanner failures)\n'
