#!/usr/bin/env bash
set -euo pipefail

pipeline=".buildkite/pipeline.yml"
public_ci_script="scripts/buildkite-public-ci.sh"
artifact_script="scripts/build-public-cli-artifact.sh"
artifact_check_script="scripts/check-public-cli-artifact.sh"
compat_check_script="scripts/check-release-binary-compat.sh"
macos_sign_script="scripts/sign-notarize-macos-release-artifact.sh"
macos_check_script="scripts/check-macos-release-signing.sh"
macos_launcher_script="scripts/run-macos-release-signing.sh"
macos_trust_script="scripts/check-macos-signing-trusted-ref.sh"
macos_attestation_script="scripts/verify-macos-release-attestation.sh"
macos_archive_attester_script="scripts/attest-macos-runtime-release-archive.sh"
macos_execution_script="scripts/verify-macos-signed-cli.sh"
macos_evidence_script="scripts/macos-release-signing-evidence.py"
macos_precommand_script="scripts/buildkite/macos_agent_pre_command.sh"
macos_ca_file="scripts/apple-developer-id-g2-ca.pem"
test -f "${pipeline}"
test -f "${public_ci_script}"
test -f "${artifact_script}"
test -f "${artifact_check_script}"
test -f "${compat_check_script}"
test -f "${macos_sign_script}"
test -f "${macos_check_script}"
test -f "${macos_launcher_script}"
test -f "${macos_trust_script}"
test -f "${macos_attestation_script}"
test -f "${macos_archive_attester_script}"
test -f "${macos_execution_script}"
test -f "${macos_evidence_script}"
test -f "${macos_precommand_script}"
test -f "${macos_ca_file}"

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
    abort "pipeline should include public smoke and gated artifact/native smoke matrices" unless steps.length == 10
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
    abort "public-smoke must run ctx CLI unit regressions" unless command.include?("//crates/ctx-cli:unit_tests")
    required_keys = %w[
      public-cli-linux-x64
      public-cli-linux-aarch64
      public-cli-windows-x64
      public-cli-freebsd-x64
      public-cli-macos-arm64
      public-cli-macos-x64
      public-cli-macos-x64-native-smoke
      public-cli-windows-x64-native-smoke
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
      condition = step["if"].to_s
      abort "#{key} must be restricted to trusted main builds" unless condition.include?(%q{build.branch == "main"}) && condition.include?("build.pull_request.id == null")
      abort "#{key} must require macOS release signing" unless command.include?("CTX_MACOS_RELEASE_SIGNING=required")
      abort "#{key} must configure the narrow Infisical signing launcher" unless command.include?("CTX_MACOS_SIGNING_SECRET_SOURCE=infisical")
      abort "#{key} must not access signing secrets before construction" if command.include?("scripts/run-macos-release-signing.sh --preflight")
      abort "#{key} must not inject secrets around a whole build" if command.include?("infisical run")
      abort "#{key} must gate native smoke explicitly" unless command.include?("CTX_PUBLIC_CLI_NATIVE_SMOKE_MATRIX")
      abort "#{key} must force the CoreML smoke" unless command.include?("--coreml")
      abort "#{key} must preserve native smoke evidence" unless command.include?("--keep-root")
      proof = "target/public-cli-native-smoke/#{key.delete_prefix("public-cli-")}/**/packaged-runtime-proof.txt"
      abort "#{key} must upload native smoke proof" unless Array(step["artifact_paths"]).include?(proof)
      diagnostics = "target/public-cli-native-smoke/#{key.delete_prefix("public-cli-")}/**/daemon-smoke.log"
      abort "#{key} must upload native smoke failure diagnostics" unless Array(step["artifact_paths"]).include?(diagnostics)
      evidence = "target/public-cli-artifacts/ctx-#{key.delete_prefix("public-cli-")}.signing.json"
      abort "#{key} must upload CLI signing evidence" unless Array(step["artifact_paths"]).include?(evidence)
      attestation = "target/public-cli-artifacts/ctx-#{key.delete_prefix("public-cli-")}.attestation.cms"
      abort "#{key} must upload the CLI cryptographic attestation" unless Array(step["artifact_paths"]).include?(attestation)
      execution = "target/public-cli-artifacts/ctx-#{key.delete_prefix("public-cli-")}.execution.txt"
      abort "#{key} must upload signed CLI execution evidence" unless Array(step["artifact_paths"]).include?(execution)
    end
    macos_arm64 = steps.find { |candidate| candidate.is_a?(Hash) && candidate["key"] == "public-cli-macos-arm64" }
    abort "macos-arm64 smoke must require authoritative evidence" unless macos_arm64["command"].to_s.include?("runtime_authority=authoritative")
    macos_x64 = steps.find { |candidate| candidate.is_a?(Hash) && candidate["key"] == "public-cli-macos-x64" }
    abort "macos-x64 Rosetta smoke must remain explicitly non-authoritative" unless macos_x64["command"].to_s.include?("runtime_authority=non_authoritative")
    macos_arm64_paths = Array(macos_arm64["artifact_paths"])
    abort "macos-arm64 must upload runtime signing evidence" unless macos_arm64_paths.include?("target/public-cli-artifacts/ctx-onnxruntime-macos-arm64.signing.json")
    abort "macos-arm64 must upload runtime cryptographic attestation" unless macos_arm64_paths.include?("target/public-cli-artifacts/ctx-onnxruntime-macos-arm64.attestation.cms")
    abort "macos-arm64 must upload final runtime archive authorization" unless macos_arm64_paths.include?("target/public-cli-artifacts/ctx-onnxruntime-macos-arm64.release-attestation.cms")
    arm_command = macos_arm64["command"].to_s
    arm_runtime_build = arm_command.index("scripts/build-onnxruntime-sidecar.sh macos-arm64")
    arm_transcode = arm_command.index("scripts/stage-github-release-assets.sh --transcode-runtime macos-arm64")
    arm_smoke = arm_command.index("--runtime-archive target/public-cli-artifacts/ctx-onnxruntime-macos-arm64.tar.gz")
    abort "macos-arm64 native runtime smoke must follow signed final packaging" unless arm_runtime_build && arm_transcode && arm_smoke && arm_runtime_build < arm_transcode && arm_transcode < arm_smoke
    abort "macos-arm64 must allow two bounded notarizations" unless macos_arm64["timeout_in_minutes"].to_i >= 120
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
    macos_x64_native = steps.find { |candidate| candidate.is_a?(Hash) && candidate["key"] == "public-cli-macos-x64-native-smoke" }
    native_command = macos_x64_native["command"].to_s
    native_condition = macos_x64_native["if"].to_s
    abort "macos-x64 native lane must be restricted to trusted main builds" unless native_condition.include?(%q{build.branch == "main"}) && native_condition.include?("build.pull_request.id == null")
    abort "macos-x64 native lane must verify the downloaded signed CLI" unless native_command.include?("scripts/check-macos-release-signing.sh") && native_command.include?("macos-x64 cli")
    abort "macos-x64 native lane must rerun the exact signed default candidate smoke" unless native_command.include?("scripts/run-native-candidate-smoke.sh")
    abort "macos-x64 native lane must configure narrow Infisical signing" unless native_command.include?("CTX_MACOS_SIGNING_SECRET_SOURCE=infisical")
    abort "macos-x64 native lane must not access signing secrets before construction" if native_command.include?("scripts/run-macos-release-signing.sh --preflight")
    abort "macos-x64 native lane must not inject secrets around a whole build" if native_command.include?("infisical run")
    abort "macos-x64 native lane must build its signed runtime" unless native_command.include?("scripts/build-onnxruntime-sidecar.sh macos-x64")
    native_paths = Array(macos_x64_native["artifact_paths"])
    abort "macos-x64 native lane must upload runtime signing evidence" unless native_paths.include?("target/public-cli-artifacts/ctx-onnxruntime-macos-x64.signing.json")
    abort "macos-x64 native lane must upload runtime cryptographic attestation" unless native_paths.include?("target/public-cli-artifacts/ctx-onnxruntime-macos-x64.attestation.cms")
    abort "macos-x64 native lane must upload final runtime archive authorization" unless native_paths.include?("target/public-cli-artifacts/ctx-onnxruntime-macos-x64.release-attestation.cms")
    native_runtime_build = native_command.index("scripts/build-onnxruntime-sidecar.sh macos-x64")
    native_transcode = native_command.index("scripts/stage-github-release-assets.sh --transcode-runtime macos-x64")
    native_runtime_smoke = native_command.index("--runtime-archive target/public-cli-artifacts/ctx-onnxruntime-macos-x64.tar.gz")
    abort "macos-x64 native runtime smoke must follow signed final packaging" unless native_runtime_build && native_transcode && native_runtime_smoke && native_runtime_build < native_transcode && native_transcode < native_runtime_smoke
    abort "macos-x64 native lane must upload default smoke evidence" unless native_paths.include?("target/public-cli-native-smoke/macos-x64-native/candidate-smoke.json")
    windows_native = steps.find { |candidate| candidate.is_a?(Hash) && candidate["key"] == "public-cli-windows-x64-native-smoke" }
    windows_native_command = windows_native["command"].to_s
    abort "windows-x64 native lane must run the PowerShell 5 candidate contract" unless windows_native_command.include?("scripts/tests/run-native-candidate-smoke-test.ps1")
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
    abort "linux-aarch64 must build on linux-arm64" unless linux_aarch64.dig("agents", "queue") == "linux-arm64"
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
  if [[ "${top_level_steps}" != "10" ]]; then
    printf 'pipeline should include public smoke and gated artifact/native smoke matrices\n' >&2
    exit 1
  fi
fi

for required in \
  'key: "public-smoke"' \
  'queue: "default"' \
  'bash scripts/buildkite-public-ci.sh -- test' \
  '//crates/ctx-cli:unit_tests' \
  '//:cargo_check' \
  '//:linux_release_construction_tests' \
  '//:macos_release_signing_tests' \
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
  'CTX_MACOS_RELEASE_SIGNING=required' \
  'CTX_MACOS_SIGNING_SECRET_SOURCE=infisical' \
  'scripts/run-macos-release-signing.sh --attest-runtime-archive' \
  'CTX_MACOS_SIGNING_SECRET_DIR' \
  '590927ab-758e-41b0-9e15-4cf070e87cf4' \
  'scripts/sign-notarize-macos-release-artifact.sh' \
  'scripts/check-macos-release-signing.sh' \
  'scripts/check-macos-signing-trusted-ref.sh' \
  'scripts/verify-macos-release-attestation.sh' \
  'scripts/attest-macos-runtime-release-archive.sh' \
  'scripts/verify-macos-signed-cli.sh' \
  'signed-exact-byte-version-execution' \
  'accepted-notary-strict-codesign-attestation' \
  'refs/remotes/origin/main' \
  'BUILDKITE_PULL_REQUEST' \
  'CTX_LOCAL_MACOS_SIGNING_LIVE_TEST' \
  'F1:6C:D3:C5:4C:7F:83:CE:A4:BF:1A:3E:6A:08:19:C8:AA:A8:E4:A1:52:8F:D1:44:71:5F:35:06:43:D2:DF:3A' \
  'Developer ID Application: Profound Health Institute LLC (SJSNARH4TG)' \
  'SJSNARH4TG' \
  '-no-CApath' \
  '-no-CAstore' \
  '-ignore_critical' \
  'Code Signing EKU' \
  'Digital Signature key usage' \
  '1.2.840.113635.100.6.1.13: critical' \
  'APPLE_CODESIGN_CERT_P12_B64' \
  'APPLE_CODESIGN_CERT_PASSWORD' \
  'NOTARY_ISSUER' \
  'NOTARY_KEY_ID' \
  'NOTARY_KEY_P8_B64' \
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
    "${compat_check_script}" \
    "${macos_sign_script}" \
    "${macos_check_script}" \
    "${macos_launcher_script}" \
    "${macos_trust_script}" \
    "${macos_attestation_script}" \
    "${macos_archive_attester_script}" \
    "${macos_execution_script}" \
    "${macos_evidence_script}" \
    "${macos_precommand_script}"; do
    if grep -F -q -- "${required}" "${checked_file}"; then
      found=1
      break
    fi
  done
  if [[ "${found}" != "1" ]]; then
    printf 'pipeline or release scripts missing required snippet: %s\n' "${required}" >&2
    exit 1
  fi
done

python3 - \
  "${artifact_script}" \
  "scripts/build-onnxruntime-sidecar.sh" \
  "scripts/stage-github-release-assets.sh" <<'PY'
import sys

cli, runtime, staging = [open(path, encoding="utf-8").read() for path in sys.argv[1:]]


def require_order(label, source, *needles):
    cursor = 0
    for needle in needles:
        position = source.find(needle, cursor)
        if position < 0:
            raise SystemExit(
                f"{label} ordering contract is missing or out of order: {needles}"
            )
        cursor = position + len(needle)


require_order(
    "macOS CLI signing/hash/build-info",
    cli,
    'scripts/run-macos-release-signing.sh',
    'scripts/verify-macos-signed-cli.sh',
    'sha_file="${staged}.sha256"',
    'scripts/run-native-candidate-smoke.sh',
    'python3 scripts/write-public-cli-build-info.py',
    'scripts/check-macos-release-signing.sh',
)
require_order(
    "macOS runtime signing/archive/checksum evidence",
    runtime,
    'scripts/run-macos-release-signing.sh',
    'create_archive "${stage_dir}" "${package_path}"',
    'sha256_file "${output_dir%/}/${asset_name}"',
    'python3 scripts/macos-release-signing-evidence.py bind-archive',
    'scripts/check-macos-release-signing.sh',
)
require_order(
    "macOS release transport checksum evidence",
    staging,
    'sha256_file "${dest_path}" > "${dest_path}.sha256"',
    'python3 scripts/macos-release-signing-evidence.py bind-archive',
    'scripts/check-macos-release-signing.sh',
    'scripts/run-macos-release-signing.sh --attest-runtime-archive',
)
for source, label in ((cli, "CLI"), (runtime, "runtime")):
    if 'CTX_PUBLIC_CLI_ARTIFACT_MATRIX:-0' not in source:
        raise SystemExit(f"macOS {label} release matrix must fail closed into required signing")
if "scripts/verify-macos-release-attestation.sh" not in staging:
    raise SystemExit("final release assembly must cryptographically verify macOS attestations")
require_order(
    "native macOS final-transcode verification",
    staging,
    'mv "${dest_path}.tmp" "${dest_path}"',
    '"${platform}" runtime "${dest_path}"',
    '"${platform}" cli "${artifact_dir%/}/ctx-${platform}"',
    'scripts/run-macos-release-signing.sh --attest-runtime-archive',
)
PY

if grep -Fq 'spctl ' "${macos_sign_script}" \
  || grep -Fq 'spctl ' "${macos_check_script}"; then
  printf 'standalone macOS Mach-O verification must not require spctl app classification\n' >&2
  exit 1
fi

if grep -Fq 'scripts/run-macos-release-signing.sh --preflight' "${pipeline}"; then
  printf 'macOS release lanes must not fetch signing values before construction\n' >&2
  exit 1
fi
if grep -Fq -- '-certsout "${signer_cert}"' "${macos_attestation_script}" \
  || ! grep -Fq -- '-signer "${signer_cert}"' "${macos_attestation_script}"; then
  printf 'macOS CMS verification must validate the actual signer, not embedded certificate decoys\n' >&2
  exit 1
fi
if grep -Fq 'infisical run' "${pipeline}"; then
  printf 'macOS release builds must not run under broad Infisical injection\n' >&2
  exit 1
fi
if grep -Fq 'minimal_env+=("${secret_name}=' "${macos_launcher_script}" \
  || grep -Fq '$(cat "${secret_root}/${secret_name}")' "${macos_launcher_script}"; then
  printf 'macOS signing launcher must pass file paths, never secret values, to env -i\n' >&2
  exit 1
fi

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
  'queue: "linux-arm64"' \
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
