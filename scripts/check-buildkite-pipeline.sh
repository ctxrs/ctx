#!/usr/bin/env bash
set -euo pipefail

pipeline=".buildkite/pipeline.yml"
test -f "${pipeline}"

if command -v ruby >/dev/null 2>&1; then
  ruby -e '
    require "yaml"
    data = YAML.load_file(ARGV.fetch(0))
    abort "pipeline must have steps" unless data.is_a?(Hash) && data["steps"].is_a?(Array)
    keys = data["steps"].map { |step| step["key"] }.compact
    abort "missing search-mvp step" unless keys.include?("search-mvp")
    search = data["steps"].find { |step| step["key"] == "search-mvp" }
    command = search["command"].to_s
    install_idx = command.index("apt-get install -y zip")
    verify_idx = command.index("command -v zip")
    check_idx = command.index("./scripts/check.sh --mode=ci")
    abort "search-mvp must install zip before Bazel tests" unless install_idx
    abort "search-mvp must verify zip before Bazel tests" unless verify_idx
    abort "search-mvp must run scripts/check.sh --mode=ci" unless check_idx
    abort "search-mvp zip install must run before scripts/check.sh --mode=ci" unless install_idx < check_idx
    abort "search-mvp zip verification must run before scripts/check.sh --mode=ci" unless verify_idx < check_idx
    abort "search-mvp must explain zip is required for Bazel undeclared test output packaging" unless command.include?("zip is required for Bazel undeclared test output packaging")
  ' "${pipeline}"
fi

if ! grep -F -q 'apt-get install -y zip' "${pipeline}"; then
  printf 'pipeline must install zip before Bazel tests\n' >&2
  exit 1
fi

if ! grep -F -q 'command -v zip' "${pipeline}"; then
  printf 'pipeline must verify zip before Bazel tests\n' >&2
  exit 1
fi

if ! grep -F -q 'zip is required for Bazel undeclared test output packaging' "${pipeline}"; then
  printf 'pipeline must fail clearly when zip is unavailable\n' >&2
  exit 1
fi

if ! grep -F -q './scripts/check.sh --mode=ci' "${pipeline}"; then
  printf 'pipeline must run ./scripts/check.sh --mode=ci\n' >&2
  exit 1
fi

if command -v rg >/dev/null 2>&1; then
  if rg -n -i 'dashboard|shim|publish|pull request|hosted|ADE|ctx evidence|ctx pr' "${pipeline}"; then
    printf 'pipeline contains removed search-MVP surfaces\n' >&2
    exit 1
  fi
fi

printf 'search MVP pipeline ok\n'
