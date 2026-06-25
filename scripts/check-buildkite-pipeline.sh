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
    check_idx = command.index("./scripts/check.sh --mode=ci")
    abort "search-mvp must run scripts/check.sh --mode=ci" unless check_idx
    abort "search-mvp must install missing Ubuntu runner packages before Bazel tests" unless command.include?("apt-get install -y")
    abort "search-mvp must verify runner tools before Bazel tests" unless command.include?("command -v \"$${tool_binary}\"")

    escaped_shell_vars = [
      "$$1",
      "$$2",
      "$$3",
      "$$@",
      "$${apt_get_updated}",
      "$${tool_binary}",
      "$${apt_package}",
      "$${required_message}",
    ]
    escaped_shell_vars.each do |shell_var|
      abort "search-mvp must escape #{shell_var.sub(/\A\$/, "")} for Buildkite interpolation" unless command.include?(shell_var)
    end
    unescaped_shell_var = /(?<![$\\])\$(?:[123@]|\{(?:apt_get_updated|tool_binary|apt_package|required_message)\})/
    if (match = command.match(unescaped_shell_var))
      abort "search-mvp contains unescaped Buildkite-interpolated shell variable #{match[0]}"
    end

    {
      "zip" => {
        "package" => "zip",
        "message" => "zip is required for Bazel undeclared test output packaging",
      },
      "rg" => {
        "package" => "ripgrep",
        "message" => "ripgrep (rg) is required for CI static and package audits",
      },
    }.each do |binary, spec|
      ensure_idx = command.index("ensure_runner_tool #{binary} #{spec.fetch("package")}")
      abort "search-mvp must install and verify #{binary} before Bazel tests" unless ensure_idx
      abort "search-mvp #{binary} preflight must run before scripts/check.sh --mode=ci" unless ensure_idx < check_idx
      abort "search-mvp must explain #{binary} runner requirement" unless command.include?(spec.fetch("message"))
    end
  ' "${pipeline}"
fi

for escaped_shell_var in '$$1' '$$2' '$$3' '$$@' '$${apt_get_updated}' '$${tool_binary}' '$${apt_package}' '$${required_message}'; do
  if ! grep -F -q "${escaped_shell_var}" "${pipeline}"; then
    printf 'pipeline must escape %s for Buildkite interpolation\n' "${escaped_shell_var#\$}" >&2
    exit 1
  fi
done

if awk '
  {
    for (idx = 1; idx <= length($0); idx++) {
      prev = idx == 1 ? "" : substr($0, idx - 1, 1)
      rest = substr($0, idx)
      if (prev != "$" && prev != "\\" && rest ~ /^\$([123@]|\{(apt_get_updated|tool_binary|apt_package|required_message)\})/) {
        print
        exit 1
      }
    }
  }
' "${pipeline}"; then
  :
else
  printf 'pipeline contains unescaped Buildkite-interpolated shell variables\n' >&2
  exit 1
fi

if ! grep -F -q 'apt-get install -y "$$1"' "${pipeline}"; then
  printf 'pipeline must install missing Ubuntu runner packages before Bazel tests\n' >&2
  exit 1
fi

if ! grep -F -q 'command -v "$${tool_binary}"' "${pipeline}"; then
  printf 'pipeline must verify runner tools before Bazel tests\n' >&2
  exit 1
fi

if ! grep -F -q 'ensure_runner_tool zip zip' "${pipeline}"; then
  printf 'pipeline must install and verify zip before Bazel tests\n' >&2
  exit 1
fi

if ! grep -F -q 'zip is required for Bazel undeclared test output packaging' "${pipeline}"; then
  printf 'pipeline must fail clearly when zip is unavailable\n' >&2
  exit 1
fi

if ! grep -F -q 'ensure_runner_tool rg ripgrep' "${pipeline}"; then
  printf 'pipeline must install and verify ripgrep/rg before Bazel tests\n' >&2
  exit 1
fi

if ! grep -F -q 'ripgrep (rg) is required for CI static and package audits' "${pipeline}"; then
  printf 'pipeline must fail clearly when ripgrep/rg is unavailable\n' >&2
  exit 1
fi

if ! grep -F -q './scripts/check.sh --mode=ci' "${pipeline}"; then
  printf 'pipeline must run ./scripts/check.sh --mode=ci\n' >&2
  exit 1
fi

if ! grep -F -q 'key: "freebsd-native-release-proof"' "${pipeline}"; then
  printf 'pipeline must include the native FreeBSD release proof step\n' >&2
  exit 1
fi

for key in \
  'linux-release-artifact-smoke' \
  'macos-arm64-release-artifact-smoke' \
  'macos-x64-release-artifact-smoke' \
  'windows-x64-release-artifact-smoke'; do
  if ! grep -F -q "key: \"${key}\"" "${pipeline}"; then
    printf 'pipeline must include %s for platform artifact smoke proof\n' "${key}" >&2
    exit 1
  fi
done

for platform in linux-x64 macos-arm64 macos-x64 freebsd-x64; do
  if ! grep -F -q "./scripts/release-artifact-smoke.sh ${platform}" "${pipeline}"; then
    printf 'pipeline must run release-artifact-smoke.sh for %s\n' "${platform}" >&2
    exit 1
  fi
  if ! grep -F -q "artifacts/buildkite/release-artifact-smoke/${platform}" "${pipeline}"; then
    printf 'pipeline must export release artifact smoke evidence for %s\n' "${platform}" >&2
    exit 1
  fi
done

if ! grep -F -q '.\scripts\ci-windows.ps1 -Mode release-artifact-smoke' "${pipeline}"; then
  printf 'pipeline must run Windows release artifact smoke through scripts/ci-windows.ps1\n' >&2
  exit 1
fi

if ! grep -F -q 'queue: "macos-arm64"' "${pipeline}"; then
  printf 'macOS arm64 artifact smoke must route to queue=macos-arm64\n' >&2
  exit 1
fi

if ! grep -F -q 'queue: "macos-x64"' "${pipeline}"; then
  printf 'macOS x64 artifact smoke must route to queue=macos-x64\n' >&2
  exit 1
fi

if ! grep -F -q 'queue: "windows-x64"' "${pipeline}"; then
  printf 'Windows artifact smoke must route to queue=windows-x64\n' >&2
  exit 1
fi

if ! grep -F -q 'queue: "freebsd-x64"' "${pipeline}"; then
  printf 'FreeBSD release proof must route to queue=freebsd-x64\n' >&2
  exit 1
fi

if ! grep -F -q 'os: "freebsd"' "${pipeline}"; then
  printf 'FreeBSD release proof must require a FreeBSD agent\n' >&2
  exit 1
fi

if ! grep -F -q 'CTX_EXPECT_HOST_TRIPLE: "x86_64-unknown-freebsd"' "${pipeline}"; then
  printf 'FreeBSD release proof must fail closed on the x86_64-unknown-freebsd host triple\n' >&2
  exit 1
fi

if ! grep -F -q 'CTX_RELEASE_PLATFORM: "freebsd-x64"' "${pipeline}"; then
  printf 'FreeBSD release proof must write freebsd-x64 release evidence\n' >&2
  exit 1
fi

if ! grep -F -q 'CTX_RELEASE_TARGET_TRIPLE: "x86_64-unknown-freebsd"' "${pipeline}"; then
  printf 'FreeBSD release proof must write x86_64-unknown-freebsd artifacts\n' >&2
  exit 1
fi

if ! grep -F -q './scripts/release-dry-run.sh' "${pipeline}"; then
  printf 'FreeBSD release proof must run scripts/release-dry-run.sh\n' >&2
  exit 1
fi

if command -v rg >/dev/null 2>&1; then
  if rg -n -i 'dashboard|shim|publish|pull request|hosted|ADE|ctx evidence|ctx pr' "${pipeline}"; then
    printf 'pipeline contains removed search-MVP surfaces\n' >&2
    exit 1
  fi
fi

printf 'search MVP pipeline ok\n'
