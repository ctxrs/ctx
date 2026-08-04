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

grep_bin="$(command -v grep)"
dirname_bin="$(command -v dirname)"
tools_dir="${tmp_dir}/tools"
mkdir -p "$tools_dir"
ln -s "$grep_bin" "${tools_dir}/grep"
ln -s "$dirname_bin" "${tools_dir}/dirname"

baseline_output="${tmp_dir}/baseline.out"
if ! PATH="$tools_dir" /bin/bash \
  "${fixture_root}/scripts/check-sdk-no-publish.sh" >"$baseline_output" 2>&1; then
  cat "$baseline_output" >&2
  printf 'publish guard rejected a safe fixture without rg\n' >&2
  exit 1
fi
"$grep_bin" -Fq 'SDK publish guard passed' "$baseline_output"

command_name='cargo'
printf '%s %s\n' "$command_name" publish >"${fixture_root}/src/release.sh"
mutation_output="${tmp_dir}/mutation.out"
if PATH="$tools_dir" /bin/bash \
  "${fixture_root}/scripts/check-sdk-no-publish.sh" >"$mutation_output" 2>&1; then
  cat "$mutation_output" >&2
  printf 'publish guard accepted a live publish command without rg\n' >&2
  exit 1
fi
"$grep_bin" -Fq \
  'live SDK package-manager publish command found outside docs/policy text' \
  "$mutation_output"

printf 'harmless source\n' >"${fixture_root}/src/release.sh"
printf 'unreadable to scanner\n' >"${fixture_root}/src/scan-error.txt"
failing_tools_dir="${tmp_dir}/failing-tools"
mkdir -p "$failing_tools_dir"
ln -s "$dirname_bin" "${failing_tools_dir}/dirname"
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
if REAL_GREP="$grep_bin" PATH="$failing_tools_dir" /bin/bash \
  "${fixture_root}/scripts/check-sdk-no-publish.sh" >"$failure_output" 2>&1; then
  cat "$failure_output" >&2
  printf 'publish guard accepted an incomplete policy scan\n' >&2
  exit 1
fi
"$grep_bin" -Fq 'could not inspect every SDK publish-policy input' "$failure_output"

printf 'SDK no-publish policy tests: OK (rg-free baseline, mutation, scan failure)\n'
