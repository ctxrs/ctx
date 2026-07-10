#!/usr/bin/env bash
set -euo pipefail

pipeline=".buildkite/pipeline.yml"
public_ci_script="scripts/buildkite-public-ci.sh"
artifact_script="scripts/build-public-cli-artifact.sh"
artifact_check_script="scripts/check-public-cli-artifact.sh"
compat_check_script="scripts/check-release-binary-compat.sh"
runtime_script="scripts/build-onnxruntime-sidecar.sh"
release_stage_script="scripts/stage-github-release-assets.sh"
semantic_smoke_script="scripts/smoke-daemon-semantic-release.sh"
windows_semantic_smoke_script="scripts/smoke-daemon-semantic-release.ps1"

for required_file in \
  "${pipeline}" \
  "${public_ci_script}" \
  "${artifact_script}" \
  "${artifact_check_script}" \
  "${compat_check_script}" \
  "${runtime_script}" \
  "${release_stage_script}" \
  "${semantic_smoke_script}" \
  "${windows_semantic_smoke_script}"; do
  test -f "${required_file}"
done

if [[ -e ".github/workflows/public-ci.yml" ]]; then
  printf 'public GitHub Actions CI workflow should be migrated to Buildkite\n' >&2
  exit 1
fi

if grep -F -q 'CTX_ONNXRUNTIME_ASSET_DIR' "${pipeline}"; then
  printf 'Buildkite must transport ONNX Runtime only through explicit producer artifacts\n' >&2
  exit 1
fi

pipeline_step_contains() {
  local step="$1"
  local needle="$2"

  awk '
      in_step && /^  -[[:space:]]/ { exit }
      index($0, "key: \"" step "\"") { in_step = 1 }
      in_step && index($0, needle) { found = 1 }
      END { exit found ? 0 : 1 }
    ' step="${step}" needle="${needle}" "${pipeline}"
}

if command -v ruby >/dev/null 2>&1; then
  ruby - "${pipeline}" <<'RUBY'
require "yaml"

def exact(actual, expected, message)
  abort "#{message}: expected #{expected.inspect}, got #{actual.inspect}" unless actual == expected
end

def exact_set(actual, expected, message)
  return if actual.length == expected.length && actual.sort == expected.sort

  abort "#{message}: expected #{expected.inspect}, got #{actual.inspect}"
end

def command_lines(step)
  step.fetch("command", "").lines.map(&:strip).reject(&:empty?)
end

def artifact_downloads(step)
  step.fetch("command", "").scan(
    /^[ \t]*buildkite-agent artifact download "([^"]+)" \. --step ([A-Za-z0-9_-]+)[ \t]*$/
  )
end

def assert_agents(step, queue:, runner_class:, os:, arch:)
  expected = {
    "queue" => queue,
    "ctx-runner-class" => runner_class,
    "os" => os,
    "arch" => arch,
  }
  exact(step["agents"], expected, "#{step.fetch("key")} runner selection")
end

pipeline_path = ARGV.fetch(0)
data = YAML.load_file(pipeline_path)
abort "pipeline must have steps" unless data.is_a?(Hash) && data["steps"].is_a?(Array)
steps = data.fetch("steps")
keyed_steps = steps.select { |step| step.is_a?(Hash) && step["key"] }
duplicate_keys = keyed_steps.group_by { |step| step.fetch("key") }.select { |_key, matches| matches.length > 1 }.keys
abort "duplicate pipeline step keys: #{duplicate_keys.join(", ")}" unless duplicate_keys.empty?

step_by_key = keyed_steps.to_h { |step| [step.fetch("key"), step] }
fetch_step = lambda do |key|
  step_by_key.fetch(key) { abort "missing pipeline step #{key}" }
end

artifact_gate = 'build.env("CTX_PUBLIC_CLI_ARTIFACT_MATRIX") == "1"'
smoke_gate = artifact_gate + ' && build.env("CTX_PUBLIC_CLI_NATIVE_SMOKE_MATRIX") == "1"'

public_smoke = fetch_step.call("public-smoke")
exact(public_smoke.dig("agents", "queue"), "default", "public-smoke queue")
if public_smoke.dig("agents", "ctx-runner-class") || public_smoke.dig("agents", "os") || public_smoke.dig("agents", "arch")
  abort "public-smoke must not require self-hosted runner tags"
end
unless public_smoke["concurrency"] == 1 && public_smoke["concurrency_group"].to_s.include?("default-hosted")
  abort "public-smoke should run one hosted Linux job at a time"
end
public_smoke_command = public_smoke.fetch("command", "")
unless public_smoke_command.include?("scripts/buildkite-public-ci.sh -- test") && public_smoke_command.include?("//:cargo_check")
  abort "public-smoke must pass the hosted-safe target list to scripts/buildkite-public-ci.sh"
end

cli_binaries = {
  "linux-x64" => "ctx",
  "linux-aarch64" => "ctx-linux-aarch64",
  "windows-x64" => "ctx.exe",
  "freebsd-x64" => "ctx-freebsd-x64",
  "macos-arm64" => "ctx-macos-arm64",
  "macos-x64" => "ctx-macos-x64",
}
cli_keys = cli_binaries.keys.map { |platform| "public-cli-#{platform}" }
exact_set(
  step_by_key.keys.grep(/^public-cli-(?!release-assets$)/),
  cli_keys,
  "public CLI producer keys"
)

cli_binaries.each do |platform, binary|
  key = "public-cli-#{platform}"
  step = fetch_step.call(key)
  exact(step["if"], artifact_gate, "#{key} gate")
  exact(command_lines(step), ["scripts/build-public-cli-artifact.sh #{platform}"], "#{key} command")
  expected_artifacts = [
    "target/public-cli-artifacts/#{binary}",
    "target/public-cli-artifacts/#{binary}.sha256",
    "target/public-cli-artifacts/#{binary}.version",
  ]
  exact(Array(step["artifact_paths"]), expected_artifacts, "#{key} binary-only artifacts")
  abort "#{key} must not depend on a runtime producer" if step.key?("depends_on")
  expected_arch = platform == "linux-aarch64" ? "arm64" : "x86_64"
  assert_agents(
    step,
    queue: "release-linux-managed",
    runner_class: "release-linux-control",
    os: "linux",
    arch: expected_arch
  )
end

runtime_assets = {
  "linux-x64" => ["ctx-onnxruntime-linux-x64.tar.gz", "public-onnxruntime-official"],
  "linux-aarch64" => ["ctx-onnxruntime-linux-aarch64.tar.gz", "public-onnxruntime-official"],
  "macos-arm64" => ["ctx-onnxruntime-macos-arm64.tar.gz", "public-onnxruntime-official"],
  "macos-x64" => ["ctx-onnxruntime-macos-x64.tar.gz", "public-onnxruntime-macos-x64"],
  "windows-x64" => ["ctx-onnxruntime-windows-x64.zip", "public-onnxruntime-official"],
  "freebsd-x64" => ["ctx-onnxruntime-freebsd-x64.tar.gz", "public-onnxruntime-freebsd-x64"],
}
runtime_producers = {
  "public-onnxruntime-official" => {
    platforms: %w[linux-x64 linux-aarch64 macos-arm64 windows-x64],
    queue: "release-linux-managed",
    runner_class: "release-linux-control",
    os: "linux",
    arch: "x86_64",
    minimum_timeout: 60,
  },
  "public-onnxruntime-macos-x64" => {
    platforms: %w[macos-x64],
    queue: "release-macos-managed",
    runner_class: "release-macos-control",
    os: "macos",
    arch: "x86_64",
    minimum_timeout: 120,
  },
  "public-onnxruntime-freebsd-x64" => {
    platforms: %w[freebsd-x64],
    queue: "release-freebsd-managed",
    runner_class: "release-freebsd-control",
    os: "freebsd",
    arch: "x86_64",
    minimum_timeout: 90,
  },
}
exact_set(
  step_by_key.keys.grep(/^public-onnxruntime-/),
  runtime_producers.keys,
  "ONNX Runtime producer keys"
)

runtime_producers.each do |key, spec|
  step = fetch_step.call(key)
  exact(step["if"], artifact_gate, "#{key} gate")
  expected_commands = spec.fetch(:platforms).flat_map do |platform|
    commands = ["scripts/build-onnxruntime-sidecar.sh #{platform} target/public-cli-artifacts"]
    unless platform == "windows-x64"
      commands << "scripts/stage-github-release-assets.sh --transcode-runtime #{platform} target/public-cli-artifacts"
    end
    commands
  end
  exact(command_lines(step), expected_commands, "#{key} build commands")
  expected_artifacts = spec.fetch(:platforms).flat_map do |platform|
    asset = runtime_assets.fetch(platform).fetch(0)
    path = "target/public-cli-artifacts/#{asset}"
    [path, "#{path}.sha256"]
  end
  exact(Array(step["artifact_paths"]), expected_artifacts, "#{key} runtime artifacts")
  abort "#{key} must be an independent producer" if step.key?("depends_on")
  unless step["timeout_in_minutes"].to_i >= spec.fetch(:minimum_timeout)
    abort "#{key} timeout is too short for its runtime build/package work"
  end
  assert_agents(
    step,
    queue: spec.fetch(:queue),
    runner_class: spec.fetch(:runner_class),
    os: spec.fetch(:os),
    arch: spec.fetch(:arch)
  )
end

runtime_assets.each do |platform, (asset, producer)|
  archive_path = "target/public-cli-artifacts/#{asset}"
  expected_paths = [archive_path, "#{archive_path}.sha256"]
  owners = keyed_steps.select do |step|
    (Array(step["artifact_paths"]) & expected_paths).any?
  end
  exact(owners.map { |step| step.fetch("key") }, [producer], "#{platform} runtime artifact owner")
  exact(Array(owners.fetch(0)["artifact_paths"]) & expected_paths, expected_paths, "#{producer} #{platform} archive/checksum publication")
end

smoke_keys = runtime_assets.keys.map { |platform| "native-semantic-#{platform}" }
producer_keys = cli_keys + runtime_producers.keys
fan_in = fetch_step.call("public-cli-release-assets")
exact(fan_in["if"], smoke_gate, "public-cli-release-assets smoke gate")
exact_set(Array(fan_in["depends_on"]), producer_keys + smoke_keys, "public-cli-release-assets smoke-gated dependencies")
assert_agents(
  fan_in,
  queue: "release-linux-managed",
  runner_class: "release-linux-control",
  os: "linux",
  arch: "x86_64"
)
unless fan_in["timeout_in_minutes"].to_i >= 60
  abort "public-cli-release-assets timeout must allow validation of every platform"
end
exact(Array(fan_in["artifact_paths"]), ["target/github-release-assets/**"], "release asset publication")

fan_in_lines = command_lines(fan_in)
unless fan_in_lines.include?("rm -rf target/public-cli-artifacts target/github-release-assets") &&
       fan_in_lines.include?("mkdir -p target/public-cli-artifacts")
  abort "public-cli-release-assets must begin from clean artifact and release directories"
end

expected_fan_in_downloads = []
cli_binaries.each do |platform, binary|
  producer = "public-cli-#{platform}"
  path = "target/public-cli-artifacts/#{binary}"
  expected_fan_in_downloads.concat([
    [path, producer],
    ["#{path}.sha256", producer],
    ["#{path}.version", producer],
  ])
end
runtime_assets.each_value do |asset, producer|
  path = "target/public-cli-artifacts/#{asset}"
  expected_fan_in_downloads.concat([[path, producer], ["#{path}.sha256", producer]])
end
exact_set(
  artifact_downloads(fan_in),
  expected_fan_in_downloads,
  "public-cli-release-assets exact producer downloads"
)

expected_validations = cli_binaries.keys.map do |platform|
  "scripts/check-public-cli-artifact.sh #{platform} target/public-cli-artifacts"
end
actual_validations = fan_in_lines.grep(/^scripts\/check-public-cli-artifact\.sh /)
exact_set(actual_validations, expected_validations, "public-cli-release-assets binary validations")
exact(
  fan_in_lines.count { |line| line == "CTX_ONNXRUNTIME_ASSET_REQUIRED=1 scripts/stage-github-release-assets.sh target/public-cli-artifacts target/github-release-assets" },
  1,
  "public-cli-release-assets complete release staging command count"
)

exact_set(step_by_key.keys.grep(/^native-semantic-/), smoke_keys, "native semantic smoke keys")
smoke_agents = {
  "linux-x64" => ["release-linux-managed", "release-linux-control", "linux", "x86_64"],
  "linux-aarch64" => ["release-linux-managed", "release-linux-control", "linux", "arm64"],
  "macos-arm64" => ["release-macos-managed", "release-macos-control", "macos", "arm64"],
  "macos-x64" => ["release-macos-managed", "release-macos-control", "macos", "x86_64"],
  "windows-x64" => ["release-windows-managed", "release-windows-control", "windows", "x86_64"],
  "freebsd-x64" => ["release-freebsd-managed", "release-freebsd-control", "freebsd", "x86_64"],
}

runtime_assets.each do |platform, (asset, runtime_producer)|
  key = "native-semantic-#{platform}"
  cli_producer = "public-cli-#{platform}"
  binary = cli_binaries.fetch(platform)
  step = fetch_step.call(key)
  exact(step["if"], smoke_gate, "#{key} gate")
  exact_set(Array(step["depends_on"]), [cli_producer, runtime_producer], "#{key} dependencies")
  binary_path = "target/public-cli-artifacts/#{binary}"
  runtime_path = "target/public-cli-artifacts/#{asset}"
  expected_downloads = [
    [binary_path, cli_producer],
    [runtime_path, runtime_producer],
    ["#{runtime_path}.sha256", runtime_producer],
  ]
  exact_set(artifact_downloads(step), expected_downloads, "#{key} exact producer downloads")
  runtime_args = if platform == "windows-x64"
    "-RuntimeArchive #{runtime_path} -RuntimePlatform #{platform}"
  else
    "--runtime-archive #{runtime_path} --runtime-platform #{platform}"
  end
  command = step.fetch("command", "")
  unless command.include?("smoke-daemon-semantic-release") && command.include?(runtime_args)
    abort "#{key} must pass its producer-owned runtime to the semantic smoke"
  end
  exact(Array(step["artifact_paths"]), ["target/ctx-semantic-smoke/**"], "#{key} diagnostic artifacts")
  queue, runner_class, os, arch = smoke_agents.fetch(platform)
  assert_agents(step, queue: queue, runner_class: runner_class, os: os, arch: arch)
end
RUBY
else
  printf 'ruby unavailable; applying portable pipeline relationship checks\n' >&2
  for required_key in \
    public-smoke \
    public-cli-linux-x64 \
    public-cli-linux-aarch64 \
    public-cli-windows-x64 \
    public-cli-freebsd-x64 \
    public-cli-macos-arm64 \
    public-cli-macos-x64 \
    public-onnxruntime-official \
    public-onnxruntime-macos-x64 \
    public-onnxruntime-freebsd-x64 \
    public-cli-release-assets \
    native-semantic-linux-x64 \
    native-semantic-linux-aarch64 \
    native-semantic-macos-arm64 \
    native-semantic-macos-x64 \
    native-semantic-windows-x64 \
    native-semantic-freebsd-x64; do
    grep -F -q "key: \"${required_key}\"" "${pipeline}" || {
      printf 'missing pipeline step: %s\n' "${required_key}" >&2
      exit 1
    }
  done
fi

while IFS='|' read -r platform binary; do
  cli_step="public-cli-${platform}"
  binary_path="target/public-cli-artifacts/${binary}"
  for required in \
    'build.env("CTX_PUBLIC_CLI_ARTIFACT_MATRIX") == "1"' \
    "scripts/build-public-cli-artifact.sh ${platform}" \
    "      - \"${binary_path}\"" \
    "      - \"${binary_path}.sha256\"" \
    "      - \"${binary_path}.version\""; do
    if ! pipeline_step_contains "${cli_step}" "${required}"; then
      printf '%s missing binary-only producer contract: %s\n' "${cli_step}" "${required}" >&2
      exit 1
    fi
  done
  if pipeline_step_contains "${cli_step}" 'ctx-onnxruntime-' || \
     pipeline_step_contains "${cli_step}" 'CTX_ONNXRUNTIME_'; then
    printf '%s must publish only its CLI binary and metadata\n' "${cli_step}" >&2
    exit 1
  fi

  fan_in_step="public-cli-release-assets"
  if ! pipeline_step_contains "${fan_in_step}" "      - \"${cli_step}\""; then
    printf '%s missing CLI producer dependency: %s\n' "${fan_in_step}" "${cli_step}" >&2
    exit 1
  fi
  for suffix in '' '.sha256' '.version'; do
    download="artifact download \"${binary_path}${suffix}\" . --step ${cli_step}"
    if ! pipeline_step_contains "${fan_in_step}" "${download}"; then
      printf '%s missing exact CLI producer download: %s\n' "${fan_in_step}" "${download}" >&2
      exit 1
    fi
  done
  validation="scripts/check-public-cli-artifact.sh ${platform} target/public-cli-artifacts"
  if ! pipeline_step_contains "${fan_in_step}" "${validation}"; then
    printf '%s missing binary validation: %s\n' "${fan_in_step}" "${validation}" >&2
    exit 1
  fi
done <<'CLI_MATRIX'
linux-x64|ctx
linux-aarch64|ctx-linux-aarch64
windows-x64|ctx.exe
freebsd-x64|ctx-freebsd-x64
macos-arm64|ctx-macos-arm64
macos-x64|ctx-macos-x64
CLI_MATRIX

while IFS='|' read -r platform asset runtime_producer; do
  archive_path="target/public-cli-artifacts/${asset}"
  cli_producer="public-cli-${platform}"
  smoke_step="native-semantic-${platform}"

  for required in \
    'build.env("CTX_PUBLIC_CLI_ARTIFACT_MATRIX") == "1"' \
    "scripts/build-onnxruntime-sidecar.sh ${platform} target/public-cli-artifacts" \
    "      - \"${archive_path}\"" \
    "      - \"${archive_path}.sha256\""; do
    if ! pipeline_step_contains "${runtime_producer}" "${required}"; then
      printf '%s missing runtime producer contract: %s\n' "${runtime_producer}" "${required}" >&2
      exit 1
    fi
  done
  if [[ "${platform}" != "windows-x64" ]] && \
    ! pipeline_step_contains "${runtime_producer}" \
      "scripts/stage-github-release-assets.sh --transcode-runtime ${platform} target/public-cli-artifacts"; then
    printf '%s missing deterministic .tar.gz transcode for %s\n' "${runtime_producer}" "${platform}" >&2
    exit 1
  fi

  if ! pipeline_step_contains "public-cli-release-assets" "      - \"${runtime_producer}\""; then
    printf 'public-cli-release-assets missing runtime producer dependency: %s\n' "${runtime_producer}" >&2
    exit 1
  fi
  for suffix in '' '.sha256'; do
    download="artifact download \"${archive_path}${suffix}\" . --step ${runtime_producer}"
    if ! pipeline_step_contains "public-cli-release-assets" "${download}"; then
      printf 'public-cli-release-assets missing exact runtime producer download: %s\n' "${download}" >&2
      exit 1
    fi
    if ! pipeline_step_contains "${smoke_step}" "${download}"; then
      printf '%s missing exact runtime producer download: %s\n' "${smoke_step}" "${download}" >&2
      exit 1
    fi
  done

  for dependency in "${cli_producer}" "${runtime_producer}"; do
    if ! pipeline_step_contains "${smoke_step}" "      - \"${dependency}\""; then
      printf '%s missing producer dependency: %s\n' "${smoke_step}" "${dependency}" >&2
      exit 1
    fi
  done
  if ! pipeline_step_contains "public-cli-release-assets" "      - \"${smoke_step}\""; then
    printf 'public-cli-release-assets missing native smoke dependency: %s\n' "${smoke_step}" >&2
    exit 1
  fi
  for gate_var in CTX_PUBLIC_CLI_ARTIFACT_MATRIX CTX_PUBLIC_CLI_NATIVE_SMOKE_MATRIX; do
    if ! pipeline_step_contains "${smoke_step}" "${gate_var}"; then
      printf '%s missing matrix gate: %s\n' "${smoke_step}" "${gate_var}" >&2
      exit 1
    fi
  done
  if pipeline_step_contains "${smoke_step}" "artifact download \"${archive_path}\" . --step ${cli_producer}"; then
    printf '%s must not download its runtime from the CLI producer\n' "${smoke_step}" >&2
    exit 1
  fi
done <<'RUNTIME_MATRIX'
linux-x64|ctx-onnxruntime-linux-x64.tar.gz|public-onnxruntime-official
linux-aarch64|ctx-onnxruntime-linux-aarch64.tar.gz|public-onnxruntime-official
macos-arm64|ctx-onnxruntime-macos-arm64.tar.gz|public-onnxruntime-official
macos-x64|ctx-onnxruntime-macos-x64.tar.gz|public-onnxruntime-macos-x64
windows-x64|ctx-onnxruntime-windows-x64.zip|public-onnxruntime-official
freebsd-x64|ctx-onnxruntime-freebsd-x64.tar.gz|public-onnxruntime-freebsd-x64
RUNTIME_MATRIX

for required in \
  'build.env("CTX_PUBLIC_CLI_ARTIFACT_MATRIX") == "1"' \
  'build.env("CTX_PUBLIC_CLI_NATIVE_SMOKE_MATRIX") == "1"' \
  'rm -rf target/public-cli-artifacts target/github-release-assets' \
  'mkdir -p target/public-cli-artifacts' \
  'CTX_ONNXRUNTIME_ASSET_REQUIRED=1 scripts/stage-github-release-assets.sh target/public-cli-artifacts target/github-release-assets' \
  '      - "target/github-release-assets/**"'; do
  if ! pipeline_step_contains "public-cli-release-assets" "${required}"; then
    printf 'public-cli-release-assets missing clean fan-in contract: %s\n' "${required}" >&2
    exit 1
  fi
done

for required in \
  'key: "public-smoke"' \
  'queue: "default"' \
  'bash scripts/buildkite-public-ci.sh -- test' \
  '//:cargo_check' \
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
  'cargo zigbuild -p ctx --release --target "${build_target}" --locked' \
  'LINUX_GLIBC_BASELINE="2.39"' \
  'LINUX_RELEASE_IMAGE_UBUNTU="24.04"' \
  'scripts/check-release-binary-compat.sh' \
  'LINUX_GLIBC_MAX_VERSION="2.39"' \
  'scripts/docker/linux-release.Dockerfile' \
  'CTX_PUBLIC_CLI_IN_CONTAINER=1' \
  'MACOS_DEPLOYMENT_TARGET="13.0"' \
  'CARGO_ZIGBUILD_VERSION="0.23.0"' \
  'CROSS_VERSION="0.2.5"' \
  'ensure_rust_toolchain' \
  'export RUSTUP_TOOLCHAIN="${rust_toolchain}"' \
  'cargo install cargo-zigbuild --version "${CARGO_ZIGBUILD_VERSION}" --locked --force' \
  'cargo install cross --version "${CROSS_VERSION}" --locked --force' \
  '[[ "$(zig version)" == "${ZIG_VERSION}" ]]' \
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

for required in \
  'ONNXRUNTIME_VERSION="1.27.0"' \
  'ONNXRUNTIME_SOURCE_SHA256="b41d09905a3c2f3a25709d1dcce8ef3942a4c2799d1046f74be7b6bbebc45e6a"' \
  'upstream_sha256="547e40a48f1fe73e3f812d7c88a948612c23f896b91e4e2ee1e232d7b468246f"' \
  'upstream_sha256="3e4d83ac06924a32a07b6d7f91ce6f852876153fc0bbdf931bf517a140bfbe48"' \
  'upstream_sha256="545e81c58152353acb0d1e8bd6ce4b62f830c0961f5b3acfedc790ffd76e477a"' \
  'upstream_sha256="c5c81710938e68079ff1a192b04897faabe4b43830d48f39f27ecd4e16138bfc"'; do
  if ! grep -F -q -- "${required}" "${runtime_script}"; then
    printf 'ONNX Runtime sidecar builder missing pinned source/repack input: %s\n' "${required}" >&2
    exit 1
  fi
done

if grep -F -q -- '--provider' "${semantic_smoke_script}" "${windows_semantic_smoke_script}"; then
  printf 'strict semantic release smokes must not pass provider filters\n' >&2
  exit 1
fi

for required in \
  'runtime_version="1.27.0"' \
  '--runtime-archive' \
  '--runtime-platform' \
  '"CTX_RUNTIME_DIR=${runtime_root}"' \
  '--artifact-dir "${release_artifact_dir}"' \
  'loader_overrides=unset' \
  'run_ctx search "${query}" --backend semantic --refresh off --json'; do
  if ! grep -F -q -- "${required}" "${semantic_smoke_script}"; then
    printf 'Unix semantic smoke missing packaged-runtime proof: %s\n' "${required}" >&2
    exit 1
  fi
done

for required in \
  '$runtimeVersion = "1.27.0"' \
  '[string]$RuntimeArchive' \
  '[string]$RuntimePlatform' \
  'Set-ProcessEnvironmentVariable -Name "CTX_RUNTIME_DIR" -Value $runtimeRoot' \
  '-ArtifactDir $releaseArtifactDir' \
  '"loader_overrides=unset"' \
  '@("search", $query, "--backend", "semantic", "--refresh", "off", "--json")'; do
  if ! grep -F -q -- "${required}" "${windows_semantic_smoke_script}"; then
    printf 'Windows semantic smoke missing packaged-runtime proof: %s\n' "${required}" >&2
    exit 1
  fi
done

for forbidden in \
  '"CTX_ONNXRUNTIME_DYLIB=${runtime_dylib}"' \
  '"ORT_DYLIB_PATH=${runtime_dylib}"' \
  '"LD_LIBRARY_PATH=${runtime_library_dir}"' \
  '"DYLD_LIBRARY_PATH=${runtime_library_dir}"'; do
  if grep -F -q -- "${forbidden}" "${semantic_smoke_script}"; then
    printf 'Unix semantic smoke must use only CTX_RUNTIME_DIR for runtime discovery: %s\n' "${forbidden}" >&2
    exit 1
  fi
done

for forbidden in \
  'Set-ProcessEnvironmentVariable -Name "CTX_ONNXRUNTIME_DYLIB" -Value' \
  'Set-ProcessEnvironmentVariable -Name "ORT_DYLIB_PATH" -Value' \
  'Set-ProcessEnvironmentVariable -Name "CTX_ONNXRUNTIME_DIR" -Value' \
  'Set-ProcessEnvironmentVariable -Name "CTX_ONNXRUNTIME_CACHE_DIR" -Value'; do
  if grep -F -q -- "${forbidden}" "${windows_semantic_smoke_script}"; then
    printf 'Windows semantic smoke must use only CTX_RUNTIME_DIR for runtime discovery: %s\n' "${forbidden}" >&2
    exit 1
  fi
done

if grep -F -q 'golang-go' "${public_ci_script}"; then
  printf 'Buildkite hosted public CI must install pinned Go instead of Ubuntu golang-go\n' >&2
  exit 1
fi

for required in \
  '"manager": "ctx-explicit-metadata-installer"' \
  '"metadata_trust": "explicit-unsigned"' \
  'with tarfile.open(archive, "r:gz") as bundle:' \
  'runtime archive entries do not exactly match the expected layout' \
  '--artifact-dir'; do
  if ! grep -F -q -- "${required}" scripts/dev-install-from-metadata.sh; then
    printf 'Unix explicit-metadata installer missing hardened runtime contract: %s\n' "${required}" >&2
    exit 1
  fi
done
if grep -F -q -- 'zstd' scripts/dev-install-from-metadata.sh; then
  printf 'Unix explicit-metadata installer must not depend on zstd\n' >&2
  exit 1
fi

for required in \
  'manager = "ctx-explicit-metadata-installer"' \
  'metadata_trust = "explicit-unsigned"' \
  'function Expand-WindowsRuntimeArchive' \
  'runtime archive entries do not exactly match the expected layout' \
  '[string]$ArtifactDir = ""'; do
  if ! grep -F -q -- "${required}" scripts/install.ps1; then
    printf 'Windows explicit-metadata installer missing hardened runtime contract: %s\n' "${required}" >&2
    exit 1
  fi
done

if awk '
    index($0, "key: \"public-smoke\"") { in_step = 1 }
    in_step && /^  - label:/ && index($0, "public smoke gate") == 0 { in_step = 0 }
    in_step && /release-linux-managed|ctx-runner-class|arch:|os:/ { found = 1 }
    END { exit found ? 0 : 1 }
  ' "${pipeline}"; then
  printf 'public-smoke must not target self-hosted runner tags\n' >&2
  exit 1
fi

if grep -F -q 'ctx-mac-gui-shared-arm64' "${pipeline}"; then
  printf 'public CLI artifact matrix must not use the scarce Mac GUI queue\n' >&2
  exit 1
fi

if grep -E -q 'release-artifact|r2-|provider-live|OpenRouter|completion-certificate|freebsd-native-release-proof|CTX_PUBLIC_CLI_PERF_GATES|--mode=perf|public-perf' "${pipeline}"; then
  printf 'pipeline contains non-smoke release or provider-live wiring\n' >&2
  exit 1
fi

printf 'Buildkite pipeline check ok\n'
