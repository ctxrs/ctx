#!/usr/bin/env bash
set -euo pipefail

pipeline=".buildkite/pipeline.yml"
test -f "${pipeline}"

if command -v ruby >/dev/null 2>&1; then
  ruby -e '
    require "yaml"
    data = YAML.load_file(ARGV.fetch(0))
    abort "pipeline must have steps" unless data.is_a?(Hash) && data["steps"].is_a?(Array)
    steps = data["steps"]
    abort "pipeline should only contain the public smoke step" unless steps.length == 1
    smoke = steps.fetch(0)
    abort "pipeline step must be a mapping" unless smoke.is_a?(Hash)
    abort "pipeline public smoke step must be keyed" unless smoke.key?("key")
    abort "missing public-smoke step" unless smoke["key"] == "public-smoke"
    command = smoke["command"].to_s
    abort "public-smoke must run scripts/check.sh --mode=ci" unless command.include?("./scripts/check.sh --mode=ci")
    abort "public-smoke must install missing Ubuntu runner packages before Bazel tests" unless command.include?("apt-get install -y")
    abort "public-smoke must verify runner tools before Bazel tests" unless command.include?("command -v \"$${tool_binary}\"")
  ' "${pipeline}"
else
  top_level_steps="$(
    awk '
      /^steps:[[:space:]]*$/ { in_steps = 1; next }
      /^[^[:space:]]/ { in_steps = 0 }
      in_steps && /^  -[[:space:]]/ { count++ }
      END { print count + 0 }
    ' "${pipeline}"
  )"
  if [[ "${top_level_steps}" != "1" ]]; then
    printf 'pipeline should only contain the public smoke step\n' >&2
    exit 1
  fi
fi

for required in \
  'key: "public-smoke"' \
  'ensure_runner_tool zip zip' \
  'ensure_runner_tool rg ripgrep' \
  './scripts/check.sh --mode=ci'; do
  if ! grep -F -q "${required}" "${pipeline}"; then
    printf 'pipeline missing required snippet: %s\n' "${required}" >&2
    exit 1
  fi
done

if grep -E -q 'release-artifact|release-linux|r2-|provider-live|OpenRouter|completion-certificate|freebsd-native-release-proof' "${pipeline}"; then
  printf 'pipeline contains non-smoke release or provider-live wiring\n' >&2
  exit 1
fi

printf 'Buildkite pipeline check ok\n'
