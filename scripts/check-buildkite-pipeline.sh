#!/usr/bin/env bash
set -euo pipefail

pipeline=".buildkite/pipeline.yml"
public_ci_script="scripts/buildkite-public-ci.sh"
artifact_script="scripts/build-public-cli-artifact.sh"
artifact_check_script="scripts/check-public-cli-artifact.sh"
compat_check_script="scripts/check-release-binary-compat.sh"
test -f "${pipeline}"
test -f "${public_ci_script}"
test -f "${artifact_script}"
test -f "${artifact_check_script}"
test -f "${compat_check_script}"

if [[ -e ".github/workflows/public-ci.yml" ]]; then
  printf 'public GitHub Actions CI workflow should be migrated to Buildkite\n' >&2
  exit 1
fi

if command -v ruby >/dev/null 2>&1; then
  ruby -e '
    require "yaml"
    data = YAML.load_file(ARGV.fetch(0))
    abort "pipeline must have steps" unless data.is_a?(Hash) && data["steps"].is_a?(Array)
    steps = data["steps"]
    abort "pipeline should include public smoke and gated artifact/native smoke matrices" unless steps.length == 11
    smoke = steps.fetch(0)
    abort "pipeline step must be a mapping" unless smoke.is_a?(Hash)
    abort "pipeline public smoke step must be keyed" unless smoke.key?("key")
    abort "missing public-smoke step" unless smoke["key"] == "public-smoke"
    abort "public-smoke must use the Buildkite hosted default queue" unless smoke.dig("agents", "queue") == "default"
    abort "public-smoke must not require self-hosted runner tags" if smoke.dig("agents", "ctx-runner-class") || smoke.dig("agents", "os") || smoke.dig("agents", "arch")
    abort "public-smoke should run one hosted Linux job at a time" unless smoke["concurrency"] == 1 && smoke["concurrency_group"].to_s.include?("default-hosted")
    command = smoke["command"].to_s
    abort "public-smoke must run the Buildkite public CI script" unless command.include?("scripts/buildkite-public-ci.sh")
    abort "public-smoke must pass an explicit hosted-safe target list" unless command.include?("scripts/buildkite-public-ci.sh -- test") && command.include?("//:cargo_check")
    required_keys = %w[
      public-cli-linux-x64
      public-cli-linux-aarch64
      public-cli-windows-x64
      public-cli-freebsd-x64
      public-cli-macos-arm64
      public-cli-macos-x64
      public-cli-macos-x64-native-smoke
      public-cli-windows-x64-native-smoke
      public-cli-freebsd-x64-native-smoke
    ]
    actual_keys = steps.filter_map { |step| step["key"] if step.is_a?(Hash) }
    required_keys.each { |key| abort "missing gated artifact step #{key}" unless actual_keys.include?(key) }
    artifact_keys = required_keys.first(6)
    steps.drop(2).each do |step|
      next unless step.is_a?(Hash) && artifact_keys.include?(step["key"])
      abort "artifact step #{step["key"]} must be gated" unless step["if"].to_s.include?("CTX_PUBLIC_CLI_ARTIFACT_MATRIX")
      artifact_paths = Array(step["artifact_paths"]).map(&:to_s)
      abort "artifact step #{step["key"]} must upload build-info evidence" unless artifact_paths.any? { |path| path.end_with?(".build-info.json") }
    end
    %w[public-cli-macos-arm64 public-cli-macos-x64].each do |key|
      step = steps.find { |candidate| candidate.is_a?(Hash) && candidate["key"] == key }
      abort "missing macOS artifact step #{key}" unless step
      abort "#{key} must build on the native mac-shared queue" unless step.dig("agents", "queue") == "mac-shared"
      abort "#{key} must run on Darwin" unless step.dig("agents", "os") == "darwin"
      abort "#{key} must run on Apple Silicon" unless step.dig("agents", "arch") == "arm64"
      abort "#{key} must serialize native Mac construction" unless step["concurrency"] == 1 && step["concurrency_group"] == "ctx-public-cli-macos-native"
      command = step["command"].to_s
      abort "#{key} must gate native smoke explicitly" unless command.include?("CTX_PUBLIC_CLI_NATIVE_SMOKE_MATRIX")
      abort "#{key} must force the CoreML smoke" unless command.include?("--coreml")
      abort "#{key} must preserve native smoke evidence" unless command.include?("--keep-root")
      proof = "target/public-cli-native-smoke/#{key.delete_prefix("public-cli-")}/**/packaged-runtime-proof.txt"
      abort "#{key} must upload native smoke proof" unless Array(step["artifact_paths"]).include?(proof)
      diagnostics = "target/public-cli-native-smoke/#{key.delete_prefix("public-cli-")}/**/daemon-smoke.log"
      abort "#{key} must upload native smoke failure diagnostics" unless Array(step["artifact_paths"]).include?(diagnostics)
    end
    macos_arm64 = steps.find { |candidate| candidate.is_a?(Hash) && candidate["key"] == "public-cli-macos-arm64" }
    abort "macos-arm64 smoke must require authoritative evidence" unless macos_arm64["command"].to_s.include?("runtime_authority=authoritative")
    macos_x64 = steps.find { |candidate| candidate.is_a?(Hash) && candidate["key"] == "public-cli-macos-x64" }
    abort "macos-x64 Rosetta smoke must remain explicitly non-authoritative" unless macos_x64["command"].to_s.include?("runtime_authority=non_authoritative")
    inline_proofs = {
      "public-cli-linux-x64" => "ctx-linux-x64.native-runtime-proof.txt",
      "public-cli-linux-aarch64" => "ctx-linux-aarch64.native-runtime-proof.txt",
      "public-cli-macos-arm64" => "ctx-macos-arm64.native-runtime-proof.txt",
    }
    inline_proofs.each do |key, proof_name|
      step = steps.find { |candidate| candidate.is_a?(Hash) && candidate["key"] == key }
      command = step["command"].to_s
      abort "#{key} must gate packaged ONNX smoke" unless command.include?("CTX_PUBLIC_CLI_NATIVE_SMOKE_MATRIX") && command.include?("--runtime-archive")
      abort "#{key} must require authoritative proof" unless command.include?("runtime_authority=authoritative")
      proof_path = "target/public-cli-artifacts/#{proof_name}"
      abort "#{key} must publish #{proof_name}" unless Array(step["artifact_paths"]).include?(proof_path)
    end
    native_steps = {
      "public-cli-macos-x64-native-smoke" => ["ctx-mac-gui-shared-x64", "ctx-macos-x64.native-runtime-proof.txt"],
      "public-cli-windows-x64-native-smoke" => ["windows-x64", "ctx-windows-x64.native-runtime-proof.txt"],
      "public-cli-freebsd-x64-native-smoke" => ["freebsd-x64", "ctx-freebsd-x64.native-runtime-proof.txt"],
    }
    native_steps.each do |key, values|
      queue, proof_name = values
      step = steps.find { |candidate| candidate.is_a?(Hash) && candidate["key"] == key }
      abort "missing native semantic smoke step #{key}" unless step
      condition = step["if"].to_s
      abort "#{key} must require both artifact and native smoke gates" unless condition.include?("CTX_PUBLIC_CLI_ARTIFACT_MATRIX") && condition.include?("CTX_PUBLIC_CLI_NATIVE_SMOKE_MATRIX")
      abort "#{key} must use #{queue}" unless step.dig("agents", "queue") == queue
      command = step["command"].to_s
      abort "#{key} must consume or build an ONNX runtime archive" unless command.include?("onnxruntime")
      abort "#{key} must publish proof directly" unless command.include?("ProofOutput") || command.include?("--proof-output")
      abort "#{key} must require authoritative execution" unless command.include?("runtime_authority=authoritative") || command.include?("RequireAuthoritative")
      proof_path = "target/public-cli-artifacts/#{proof_name}"
      abort "#{key} must publish #{proof_name}" unless Array(step["artifact_paths"]).include?(proof_path)
    end
    runtime_builds = {
      "public-cli-linux-x64" => "linux-x64",
      "public-cli-linux-aarch64" => "linux-aarch64",
      "public-cli-windows-x64" => "windows-x64",
      "public-cli-macos-arm64" => "macos-arm64",
    }
    runtime_builds.each do |key, platform|
      step = steps.find { |candidate| candidate.is_a?(Hash) && candidate["key"] == key }
      abort "missing runtime-producing artifact step #{key}" unless step
      command = step["command"].to_s
      abort "#{key} must build its ONNX Runtime sidecar" unless command.include?("scripts/build-onnxruntime-sidecar.sh #{platform}")
      archive = platform == "windows-x64" ? "ctx-onnxruntime-windows-x64.zip" : "ctx-onnxruntime-#{platform}.tar.gz"
      abort "#{key} must upload #{archive}" unless Array(step["artifact_paths"]).include?("target/public-cli-artifacts/#{archive}")
      abort "#{key} must upload #{archive}.sha256" unless Array(step["artifact_paths"]).include?("target/public-cli-artifacts/#{archive}.sha256")
    end
    linux_aarch64 = steps.find { |candidate| candidate.is_a?(Hash) && candidate["key"] == "public-cli-linux-aarch64" }
    abort "missing linux-aarch64 artifact step" unless linux_aarch64
    abort "linux-aarch64 must build on release-linux-managed" unless linux_aarch64.dig("agents", "queue") == "release-linux-managed"
    abort "linux-aarch64 must run on linux" unless linux_aarch64.dig("agents", "os") == "linux"
    abort "linux-aarch64 must run on arm64" unless linux_aarch64.dig("agents", "arch") == "arm64"
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
  if [[ "${top_level_steps}" != "11" ]]; then
    printf 'pipeline should include public smoke and gated artifact/native smoke matrices\n' >&2
    exit 1
  fi
fi

for required in \
  'key: "public-smoke"' \
  'queue: "default"' \
  'bash scripts/buildkite-public-ci.sh -- test' \
  '//:cargo_check' \
  '//:linux_release_construction_tests' \
  '//:native_candidate_smoke_tests' \
  '//:release_binary_compat_tests' \
  'target/ctx-artifacts/check/**' \
  'concurrency_group: "ctx/public-smoke/default-hosted"' \
  'CTX_RUST_TOOLCHAIN: "1.88.0"' \
  'CTX_BAZELISK_VERSION: "v1.29.0"' \
  'CTX_GO_VERSION: "1.22.12"' \
  'BUILDKITE_JOB_ID' \
  'CTX_PUBLIC_CI_TOOL_ROOT' \
  'DPkg::Lock::Timeout=300' \
  'rustup toolchain install "${CTX_RUST_TOOLCHAIN}" --profile minimal --component rustfmt --component clippy' \
  'apt-get -o DPkg::Lock::Timeout=300 install -y --no-install-recommends' \
  'default-jdk-headless' \
  'install_go' \
  'go${CTX_GO_VERSION}.linux-${go_arch}.tar.gz' \
  'sha256sum -c -' \
  'python3-build' \
  'python3-venv' \
  'ctx_bootstrap_bazelisk' \
  'check_args=(--mode=ci)' \
  'bash scripts/check.sh "${check_args[@]}"' \
  'queue: "release-linux-managed"' \
  'ctx-runner-class: "release-linux-control"' \
  'CTX_PUBLIC_CLI_ARTIFACT_MATRIX' \
  'scripts/build-public-cli-artifact.sh linux-x64' \
  'scripts/build-public-cli-artifact.sh linux-aarch64' \
  'scripts/build-public-cli-artifact.sh windows-x64' \
  'scripts/build-public-cli-artifact.sh freebsd-x64' \
  'scripts/build-public-cli-artifact.sh macos-arm64' \
  'scripts/build-public-cli-artifact.sh macos-x64' \
  'cargo zigbuild -p ctx --release --target "${build_target}" --locked' \
  'LINUX_GLIBC_BASELINE="2.35"' \
  'LINUX_RELEASE_IMAGE_UBUNTU="22.04"' \
  'scripts/check-release-binary-compat.sh' \
  'check_symbol_ceiling GLIBC 2.35' \
  '.build-info.json' \
  'scripts/docker/linux-release.Dockerfile' \
  'CTX_PUBLIC_CLI_IN_CONTAINER=1' \
  'MACOS_DEPLOYMENT_TARGET="13.0"' \
  'CARGO_ZIGBUILD_VERSION' \
  'ZIG_LINUX_X64_SHA256' \
  'ZIG_LINUX_AARCH64_SHA256'; do
  found=0
  for checked_file in \
    "${pipeline}" \
    "${public_ci_script}" \
    "${artifact_script}" \
    "${artifact_check_script}" \
    "${compat_check_script}"; do
    if grep -F -q "${required}" "${checked_file}"; then
      found=1
      break
    fi
  done
  if [[ "${found}" != "1" ]]; then
    printf 'pipeline or release scripts missing required snippet: %s\n' "${required}" >&2
    exit 1
  fi
done

if grep -F -q 'golang-go' "${public_ci_script}"; then
  printf 'Buildkite hosted public CI must install pinned Go instead of Ubuntu golang-go\n' >&2
  exit 1
fi

if awk '
    index($0, "key: \"public-smoke\"") { in_step = 1 }
    in_step && /^  - label:/ && index($0, "public smoke gate") == 0 { in_step = 0 }
    in_step && /release-linux-managed|ctx-runner-class|arch:|os:/ { found = 1 }
    END { exit found ? 0 : 1 }
  ' "${pipeline}"; then
  printf 'public-smoke must not target self-hosted runner tags\n' >&2
  exit 1
fi

for required in \
  'queue: "release-linux-managed"' \
  'ctx-runner-class: "release-linux-control"' \
  'os: "linux"' \
  'arch: "arm64"'; do
  if ! awk '
      index($0, "key: \"public-cli-linux-aarch64\"") { in_step = 1 }
      in_step && /^  - label:/ && index($0, "public-cli-linux-aarch64") == 0 { in_step = 0 }
      in_step && index($0, needle) { found = 1 }
      END { exit found ? 0 : 1 }
    ' needle="${required}" "${pipeline}"; then
    printf 'linux-aarch64 artifact step missing required runner snippet: %s\n' "${required}" >&2
    exit 1
  fi
done

if grep -F -q 'ctx-mac-gui-shared-arm64' "${pipeline}"; then
  printf 'public CLI artifact matrix must not use the scarce Mac GUI queue\n' >&2
  exit 1
fi

for mac_step in public-cli-macos-arm64 public-cli-macos-x64; do
  for required in \
    'queue: "mac-shared"' \
    'os: "darwin"' \
    'arch: "arm64"' \
    'concurrency: 1' \
    'concurrency_group: "ctx-public-cli-macos-native"'; do
    if ! awk '
        index($0, "key: \"" step "\"") { in_step = 1 }
        in_step && /^  - label:/ && index($0, step) == 0 { in_step = 0 }
        in_step && index($0, needle) { found = 1 }
        END { exit found ? 0 : 1 }
      ' step="${mac_step}" needle="${required}" "${pipeline}"; then
      printf '%s artifact step missing required native Mac runner snippet: %s\n' "${mac_step}" "${required}" >&2
      exit 1
    fi
  done
done

if grep -E -q 'release-artifact|r2-|provider-live|OpenRouter|completion-certificate|freebsd-native-release-proof|CTX_PUBLIC_CLI_PERF_GATES|--mode=perf|public-perf' "${pipeline}"; then
  printf 'pipeline contains non-smoke release or provider-live wiring\n' >&2
  exit 1
fi

printf 'Buildkite pipeline check ok\n'
