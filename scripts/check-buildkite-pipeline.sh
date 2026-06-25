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

    aggregate_key = "aggregate-release-evidence"
    aggregate = data["steps"].find { |step| step["key"] == aggregate_key }
    abort "missing aggregate-release-evidence step" unless aggregate
    aggregate_idx = data["steps"].index(aggregate)
    aggregate_command = aggregate["command"].to_s
    aggregate_artifacts = Array(aggregate["artifact_paths"])
    aggregate_depends = Array(aggregate["depends_on"])

    openrouter = data["steps"].find { |step| step["key"] == "live-provider-e2e-openrouter" }
    abort "missing live-provider-e2e-openrouter step" unless openrouter
    abort "live-provider-e2e-openrouter must depend on search-mvp" unless Array(openrouter["depends_on"]).include?("search-mvp") || openrouter["depends_on"] == "search-mvp"
    abort "live-provider-e2e-openrouter must be default-off behind CTX_LIVE_PROVIDER_OPENROUTER" unless openrouter["if"].to_s.include?("CTX_LIVE_PROVIDER_OPENROUTER")
    openrouter_command = openrouter["command"].to_s
    abort "live-provider-e2e-openrouter must run the Bazel OpenRouter provider target" unless openrouter_command.include?("./scripts/bazel-test.sh provider_live_e2e_openrouter")
    abort "live-provider-e2e-openrouter must explicitly opt into generation" unless openrouter_command.include?("CTX_LIVE_PROVIDER_OPENROUTER_GENERATE=1")
    abort "live-provider-e2e-openrouter must allow the configured free-model fallback only in the explicit lane" unless openrouter_command.include?("CTX_LIVE_PROVIDER_OPENROUTER_ALLOW_DEFAULT_FREE_MODEL=1")
    openrouter_artifacts = Array(openrouter["artifact_paths"])
    abort "live-provider-e2e-openrouter must upload generated provider evidence" unless openrouter_artifacts.include?("artifacts/buildkite/provider-live-e2e/openrouter/**/*")
    abort "aggregate-release-evidence must not depend on the credential-gated OpenRouter live lane" if aggregate_depends.include?("live-provider-e2e-openrouter")

    platform_steps = {
      "linux-release-artifact-smoke" => "linux-x64",
      "macos-arm64-release-artifact-smoke" => "macos-arm64",
      "macos-x64-release-artifact-smoke" => "macos-x64",
      "windows-x64-release-artifact-smoke" => "windows-x64",
      "freebsd-native-release-proof" => "freebsd-x64",
    }
    platform_steps.each do |step_key, platform|
      abort "#{aggregate_key} must depend on #{step_key}" unless aggregate_depends.include?(step_key)
      platform_step = data["steps"].find { |step| step["key"] == step_key }
      abort "#{aggregate_key} must be after #{step_key}" unless platform_step && aggregate_idx > data["steps"].index(platform_step)
      abort "#{aggregate_key} must download #{platform} dry-run top-level artifacts from #{step_key}" unless aggregate_command.include?("artifacts/buildkite/release-dry-run/$${platform}/*") && aggregate_command.include?("download_platform_artifacts #{step_key} #{platform}")
      abort "#{aggregate_key} must download #{platform} dry-run recursive artifacts from #{step_key}" unless aggregate_command.include?("artifacts/buildkite/release-dry-run/$${platform}/**/*") && aggregate_command.include?("download_platform_artifacts #{step_key} #{platform}")
      abort "#{aggregate_key} must download #{platform} artifact-smoke top-level artifacts from #{step_key}" unless aggregate_command.include?("artifacts/buildkite/release-artifact-smoke/$${platform}/*") && aggregate_command.include?("download_platform_artifacts #{step_key} #{platform}")
      abort "#{aggregate_key} must download #{platform} artifact-smoke recursive artifacts from #{step_key}" unless aggregate_command.include?("artifacts/buildkite/release-artifact-smoke/$${platform}/**/*") && aggregate_command.include?("download_platform_artifacts #{step_key} #{platform}")
    end
    {
      "linux-x64" => "x86_64-unknown-linux-gnu",
      "macos-arm64" => "aarch64-apple-darwin",
      "macos-x64" => "x86_64-apple-darwin",
      "windows-x64" => "x86_64-pc-windows-gnu.exe",
      "freebsd-x64" => "x86_64-unknown-freebsd",
    }.each do |platform, artifact_suffix|
      [
        "artifacts/buildkite/release-dry-run/#{platform}/manifest.json",
        "artifacts/buildkite/release-dry-run/#{platform}/ctx-release-metadata.env",
        "artifacts/buildkite/release-dry-run/#{platform}/checksums.sha256",
        "artifacts/buildkite/release-dry-run/#{platform}/ctx-0.1.0-#{artifact_suffix}",
        "artifacts/buildkite/release-artifact-smoke/#{platform}/artifact-smoke.json",
        "artifacts/buildkite/release-artifact-smoke/#{platform}/commands/version.stdout",
      ].each do |path|
        abort "#{aggregate_key} must fail closed when #{path} is missing after artifact fetch" unless aggregate_command.include?("require_fetched_artifact #{path}")
      end
    end

    ordered_release_evidence_commands = [
      "CTX_ARTIFACT_DIR=artifacts/buildkite/release-candidate",
      "./scripts/release-candidate-metadata.sh artifacts/buildkite/release-dry-run",
      "CTX_ARTIFACT_DIR=artifacts/buildkite/r2-staging-smoke",
      "./scripts/release-r2-staging-smoke.sh artifacts/buildkite/release-candidate",
      "CTX_ARTIFACT_DIR=artifacts/buildkite/supply-chain",
      "./scripts/release-supply-chain-proof.sh",
      "CTX_RELEASE_R2_UPLOAD_READBACK=0",
      "CTX_RELEASE_R2_MANAGER_APPROVED=0",
      "CTX_ARTIFACT_DIR=artifacts/buildkite/r2-staging-readback",
      "./scripts/release-r2-staging-readback-proof.sh artifacts/buildkite/release-candidate",
      "./scripts/release-finished-product-evidence.sh artifacts/buildkite",
      "CTX_COMPLETION_CERTIFICATE_ALLOW_SELF_TEST_FIXTURES=0",
      "CTX_ARTIFACT_DIR=artifacts/buildkite/completion-certificate",
      "./scripts/release-completion-certificate.sh --mode=release-evidence",
    ]
    last_idx = -1
    ordered_release_evidence_commands.each do |snippet|
      idx = aggregate_command.index(snippet)
      abort "#{aggregate_key} must run #{snippet}" unless idx
      abort "#{aggregate_key} must run release evidence commands in order" unless idx > last_idx
      last_idx = idx
    end
    abort "#{aggregate_key} must explain real R2 upload/readback credential requirements" unless aggregate_command.include?("CTX_RELEASE_R2_UPLOAD_READBACK=1") && aggregate_command.include?("CTX_RELEASE_R2_MANAGER_APPROVED=1") && aggregate_command.include?("authenticated wrangler")
    abort "#{aggregate_key} must write pipeline contract evidence" unless aggregate_command.include?("artifacts/buildkite/pipeline-contract/pipeline-contract.txt")

    [
      "artifacts/buildkite/release-candidate/**/*",
      "artifacts/buildkite/r2-staging-smoke/**/*",
      "artifacts/buildkite/supply-chain/**/*",
      "artifacts/buildkite/r2-staging-readback/**/*",
      "artifacts/buildkite/finished-product/**/*",
      "artifacts/buildkite/provider-live-e2e-lanes/**/*",
      "artifacts/buildkite/completion-certificate/**/*",
      "artifacts/buildkite/pipeline-contract/*",
    ].each do |artifact_path|
      abort "#{aggregate_key} must upload #{artifact_path}" unless aggregate_artifacts.include?(artifact_path)
    end
  ' "${pipeline}"
fi

for escaped_shell_var in '$$1' '$$2' '$$3' '$$@' '$${apt_get_updated}' '$${tool_binary}' '$${apt_package}' '$${required_message}' '$${step_key}' '$${platform}' '$${required_path}' '$${BUILDKITE_BUILD_URL:-unknown-build}'; do
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
      if (prev != "$" && prev != "\\" && rest ~ /^\$([123@]|\{(apt_get_updated|tool_binary|apt_package|required_message|step_key|platform|required_path|BUILDKITE_BUILD_URL:-unknown-build)\})/) {
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

if ! grep -F -q 'key: "live-provider-e2e-openrouter"' "${pipeline}"; then
  printf 'pipeline must include the default-off OpenRouter generated provider E2E step\n' >&2
  exit 1
fi

for required in \
  'if: build.env("CTX_LIVE_PROVIDER_OPENROUTER") == "1"' \
  'CTX_LIVE_PROVIDER_OPENROUTER_GENERATE=1' \
  'CTX_LIVE_PROVIDER_OPENROUTER_ALLOW_DEFAULT_FREE_MODEL=1' \
  './scripts/bazel-test.sh provider_live_e2e_openrouter' \
  'artifacts/buildkite/provider-live-e2e/openrouter/**/*'; do
  if ! grep -F -q "${required}" "${pipeline}"; then
    printf 'pipeline OpenRouter generated provider E2E step is missing %s\n' "${required}" >&2
    exit 1
  fi
done

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

if rg -n -P '"[^"]*\$(?!(?:env|script):|\(|\{)[A-Za-z_][A-Za-z0-9_]*:' scripts/ci-windows.ps1 >&2; then
  printf 'Windows PowerShell strings must brace variables before a literal colon, for example ${artifactPath}:\n' >&2
  exit 1
fi

if ! grep -F -q 'queue: "ctx-mac-gui-shared-arm64"' "${pipeline}"; then
  printf 'macOS arm64 artifact smoke must route to queue=ctx-mac-gui-shared-arm64\n' >&2
  exit 1
fi

if ! grep -F -q 'queue: "ctx-mac-gui-shared-x64"' "${pipeline}"; then
  printf 'macOS x64 artifact smoke must route to queue=ctx-mac-gui-shared-x64\n' >&2
  exit 1
fi

for cleanup_key in macos-arm64-checkout-cleanup macos-x64-checkout-cleanup; do
  if ! grep -F -q "key: \"${cleanup_key}\"" "${pipeline}"; then
    printf 'pipeline must include %s before macOS artifact smoke\n' "${cleanup_key}" >&2
    exit 1
  fi
  if ! grep -F -q "      - \"${cleanup_key}\"" "${pipeline}"; then
    printf 'macOS artifact smoke must depend on %s\n' "${cleanup_key}" >&2
    exit 1
  fi
done

if grep -F -q 'BUILDKITE_SKIP_CHECKOUT: "true"' "${pipeline}"; then
  printf 'macOS cleanup steps must not skip checkout because the shared macOS pre-command hook requires a checked-out repo hook\n' >&2
  exit 1
fi

if ! grep -F -q 'BUILDKITE_BUILD_CHECKOUT_PATH:' "${pipeline}" ||
  ! grep -F -q 'ctx-public-release-verification-cleanup-arm64' "${pipeline}" ||
  ! grep -F -q 'ctx-public-release-verification-cleanup-x64' "${pipeline}" ||
  ! grep -F -q 'checkout_parent="$$(dirname "$${active_checkout_dir}")"' "${pipeline}" ||
  ! grep -F -q 'checkout_dir="$${checkout_parent}/$${BUILDKITE_PIPELINE_SLUG:-ctx-public-release-verification}"' "${pipeline}" ||
  ! grep -F -q 'refusing to remove active cleanup checkout' "${pipeline}" ||
  ! grep -F -q 'chflags -R nouchg,noschg "$${checkout_dir}"' "${pipeline}" ||
  ! grep -F -q 'chmod -R u+rwX "$${checkout_dir}"' "${pipeline}" ||
  ! grep -F -q 'rm -rf "$${checkout_dir}"' "${pipeline}" ||
  ! grep -F -q 'failed to remove stale checkout' "${pipeline}"; then
  printf 'macOS cleanup steps must use alternate checkout paths and fail closed when stale checkout cleanup does not complete\n' >&2
  exit 1
fi

if ! grep -F -q 'os: "darwin"' "${pipeline}"; then
  printf 'macOS artifact smoke must require darwin agents\n' >&2
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

if ! grep -F -q 'key: "aggregate-release-evidence"' "${pipeline}"; then
  printf 'pipeline must include the aggregate release evidence step\n' >&2
  exit 1
fi

for key in \
  'linux-release-artifact-smoke' \
  'macos-arm64-release-artifact-smoke' \
  'macos-x64-release-artifact-smoke' \
  'windows-x64-release-artifact-smoke' \
  'freebsd-native-release-proof'; do
  if ! grep -F -q "      - \"${key}\"" "${pipeline}"; then
    printf 'aggregate release evidence must depend on %s\n' "${key}" >&2
    exit 1
  fi
done

if ! grep -F -q 'artifacts/buildkite/release-dry-run/$${platform}/*' "${pipeline}" ||
  ! grep -F -q 'artifacts/buildkite/release-dry-run/$${platform}/**/*' "${pipeline}" ||
  ! grep -F -q 'artifacts/buildkite/release-artifact-smoke/$${platform}/*' "${pipeline}" ||
  ! grep -F -q 'artifacts/buildkite/release-artifact-smoke/$${platform}/**/*' "${pipeline}"; then
  printf 'aggregate release evidence must download release dry-run and artifact-smoke evidence\n' >&2
  exit 1
fi

if ! grep -F -q 'download_windows_platform_artifacts "$${step_key}" "$${platform}"' "${pipeline}" ||
  ! grep -F -q 'artifacts\\buildkite\\release-dry-run\\$${platform}\\*' "${pipeline}" ||
  ! grep -F -q 'artifacts\\buildkite\\release-artifact-smoke\\$${platform}\\*' "${pipeline}" ||
  ! grep -F -q 'normalize_windows_artifact_paths' "${pipeline}" ||
  ! grep -F -q 'normalized_path="$${relative_path//\\//}"' "${pipeline}"; then
  printf 'aggregate release evidence must download Windows artifacts with backslash paths and normalize them before validation\n' >&2
  exit 1
fi

for step_platform in \
  'linux-release-artifact-smoke linux-x64' \
  'macos-arm64-release-artifact-smoke macos-arm64' \
  'macos-x64-release-artifact-smoke macos-x64' \
  'windows-x64-release-artifact-smoke windows-x64' \
  'freebsd-native-release-proof freebsd-x64'; do
  if ! grep -F -q "download_platform_artifacts ${step_platform}" "${pipeline}"; then
    printf 'aggregate release evidence must download platform artifacts with %s\n' "${step_platform}" >&2
    exit 1
  fi
done

for required_artifact in \
  'artifacts/buildkite/release-dry-run/linux-x64/manifest.json' \
  'artifacts/buildkite/release-dry-run/linux-x64/ctx-release-metadata.env' \
  'artifacts/buildkite/release-dry-run/linux-x64/checksums.sha256' \
  'artifacts/buildkite/release-dry-run/linux-x64/ctx-0.1.0-x86_64-unknown-linux-gnu' \
  'artifacts/buildkite/release-artifact-smoke/linux-x64/artifact-smoke.json' \
  'artifacts/buildkite/release-artifact-smoke/linux-x64/commands/version.stdout' \
  'artifacts/buildkite/release-dry-run/macos-arm64/manifest.json' \
  'artifacts/buildkite/release-dry-run/macos-arm64/ctx-release-metadata.env' \
  'artifacts/buildkite/release-dry-run/macos-arm64/checksums.sha256' \
  'artifacts/buildkite/release-dry-run/macos-arm64/ctx-0.1.0-aarch64-apple-darwin' \
  'artifacts/buildkite/release-artifact-smoke/macos-arm64/artifact-smoke.json' \
  'artifacts/buildkite/release-artifact-smoke/macos-arm64/commands/version.stdout' \
  'artifacts/buildkite/release-dry-run/macos-x64/manifest.json' \
  'artifacts/buildkite/release-dry-run/macos-x64/ctx-release-metadata.env' \
  'artifacts/buildkite/release-dry-run/macos-x64/checksums.sha256' \
  'artifacts/buildkite/release-dry-run/macos-x64/ctx-0.1.0-x86_64-apple-darwin' \
  'artifacts/buildkite/release-artifact-smoke/macos-x64/artifact-smoke.json' \
  'artifacts/buildkite/release-artifact-smoke/macos-x64/commands/version.stdout' \
  'artifacts/buildkite/release-dry-run/windows-x64/manifest.json' \
  'artifacts/buildkite/release-dry-run/windows-x64/ctx-release-metadata.env' \
  'artifacts/buildkite/release-dry-run/windows-x64/checksums.sha256' \
  'artifacts/buildkite/release-dry-run/windows-x64/ctx-0.1.0-x86_64-pc-windows-gnu.exe' \
  'artifacts/buildkite/release-artifact-smoke/windows-x64/artifact-smoke.json' \
  'artifacts/buildkite/release-artifact-smoke/windows-x64/commands/version.stdout' \
  'artifacts/buildkite/release-dry-run/freebsd-x64/manifest.json' \
  'artifacts/buildkite/release-dry-run/freebsd-x64/ctx-release-metadata.env' \
  'artifacts/buildkite/release-dry-run/freebsd-x64/checksums.sha256' \
  'artifacts/buildkite/release-dry-run/freebsd-x64/ctx-0.1.0-x86_64-unknown-freebsd' \
  'artifacts/buildkite/release-artifact-smoke/freebsd-x64/artifact-smoke.json' \
  'artifacts/buildkite/release-artifact-smoke/freebsd-x64/commands/version.stdout'; do
  if ! grep -F -q "require_fetched_artifact ${required_artifact}" "${pipeline}"; then
    printf 'aggregate release evidence must fail closed when %s is missing after artifact fetch\n' "${required_artifact}" >&2
    exit 1
  fi
done

for required in \
  'CTX_ARTIFACT_DIR=artifacts/buildkite/release-candidate' \
  './scripts/release-candidate-metadata.sh artifacts/buildkite/release-dry-run' \
  'CTX_ARTIFACT_DIR=artifacts/buildkite/r2-staging-smoke' \
  './scripts/release-r2-staging-smoke.sh artifacts/buildkite/release-candidate' \
  'CTX_ARTIFACT_DIR=artifacts/buildkite/supply-chain' \
  './scripts/release-supply-chain-proof.sh' \
  'CTX_RELEASE_R2_UPLOAD_READBACK=0' \
  'CTX_RELEASE_R2_MANAGER_APPROVED=0' \
  'CTX_ARTIFACT_DIR=artifacts/buildkite/r2-staging-readback' \
  './scripts/release-r2-staging-readback-proof.sh artifacts/buildkite/release-candidate' \
  './scripts/release-finished-product-evidence.sh artifacts/buildkite' \
  'CTX_COMPLETION_CERTIFICATE_ALLOW_SELF_TEST_FIXTURES=0' \
  'CTX_ARTIFACT_DIR=artifacts/buildkite/completion-certificate' \
  './scripts/release-completion-certificate.sh --mode=release-evidence'; do
  if ! grep -F -q "${required}" "${pipeline}"; then
    printf 'aggregate release evidence must run %s\n' "${required}" >&2
    exit 1
  fi
done

for artifact_path in \
  'artifacts/buildkite/release-candidate/**/*' \
  'artifacts/buildkite/r2-staging-smoke/**/*' \
  'artifacts/buildkite/supply-chain/**/*' \
  'artifacts/buildkite/r2-staging-readback/**/*' \
  'artifacts/buildkite/finished-product/**/*' \
  'artifacts/buildkite/provider-live-e2e-lanes/**/*' \
  'artifacts/buildkite/completion-certificate/**/*' \
  'artifacts/buildkite/pipeline-contract/*'; do
  if ! grep -F -q "${artifact_path}" "${pipeline}"; then
    printf 'aggregate release evidence must upload %s\n' "${artifact_path}" >&2
    exit 1
  fi
done

if ! grep -F -q 'CTX_RELEASE_R2_UPLOAD_READBACK=1' "${pipeline}" ||
  ! grep -F -q 'CTX_RELEASE_R2_MANAGER_APPROVED=1' "${pipeline}" ||
  ! grep -F -q 'authenticated wrangler' "${pipeline}"; then
  printf 'aggregate release evidence must state real R2 upload/readback credential requirements\n' >&2
  exit 1
fi

if command -v rg >/dev/null 2>&1; then
  if rg -n -i 'dashboard|shim|publish|pull request|hosted|ADE|ctx evidence|ctx pr' "${pipeline}"; then
    printf 'pipeline contains removed search-MVP surfaces\n' >&2
    exit 1
  fi
fi

printf 'search MVP pipeline ok\n'
