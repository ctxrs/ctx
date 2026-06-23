#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/ci-common.sh
source "${script_dir}/ci-common.sh"

write_certificate() {
  local out_dir="$1"
  local markdown json generated_at commit branch build_url

  mkdir -p "${out_dir}"
  markdown="${out_dir}/work-recorder-completion-certificate.md"
  json="${out_dir}/work-recorder-completion-certificate.json"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"
  build_url="${BUILDKITE_BUILD_URL:-local}"

  cat > "${markdown}" <<EOF
# Work Recorder Completion Certificate

- Schema version: \`1\`
- Program: \`work-recorder-finished-product\`
- Repository: \`ctxrs/ctx\`
- Git commit: \`${commit}\`
- Git branch: \`${branch}\`
- Buildkite build: \`${build_url}\`
- Generated at Unix seconds: \`${generated_at}\`
- Publishing status: \`false\`

## Required Evidence

- Pipeline contract artifact: \`artifacts/buildkite/pipeline-contract/pipeline-contract.txt\`
- Linux x64 release dry-run manifest: \`artifacts/buildkite/release-dry-run/linux-x64/manifest.json\`
- macOS arm64 release dry-run manifest: \`artifacts/buildkite/release-dry-run/macos-arm64/manifest.json\`
- macOS x64 release dry-run manifest: \`artifacts/buildkite/release-dry-run/macos-x64/manifest.json\`
- Windows x64 release dry-run manifest: \`artifacts/buildkite/release-dry-run/windows-x64/manifest.json\`
- FreeBSD x64 blocker artifact: \`artifacts/buildkite/release-blockers/freebsd-x64/freebsd-x64-blocker.json\`
- Release install documentation: \`docs/release-install.md\`
- Release supply-chain documentation: \`docs/release-supply-chain.md\`

## External Release Blockers

- FreeBSD native release lane requires a documented native \`freebsd-x64\` Buildkite queue or a separately proven cross-build lane.
- Production release publication requires final release metadata with non-placeholder SHA-256 checksums for every published artifact.
- Signing, notarization, SBOM publication, and provenance publication require configured external credentials and policy approval.
EOF

  cat > "${json}" <<EOF
{
  "schema_version": 1,
  "kind": "work_recorder_completion_certificate",
  "program": "work-recorder-finished-product",
  "repository": "ctxrs/ctx",
  "publishing": false,
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "buildkite_build_url": "$(ctx_json_escape "${build_url}")",
  "generated_at_unix_s": ${generated_at},
  "required_evidence": {
    "pipeline_contract": "artifacts/buildkite/pipeline-contract/pipeline-contract.txt",
    "release_dry_run_linux_x64": "artifacts/buildkite/release-dry-run/linux-x64/manifest.json",
    "release_dry_run_macos_arm64": "artifacts/buildkite/release-dry-run/macos-arm64/manifest.json",
    "release_dry_run_macos_x64": "artifacts/buildkite/release-dry-run/macos-x64/manifest.json",
    "release_dry_run_windows_x64": "artifacts/buildkite/release-dry-run/windows-x64/manifest.json",
    "freebsd_x64_blocker": "artifacts/buildkite/release-blockers/freebsd-x64/freebsd-x64-blocker.json",
    "release_install_docs": "docs/release-install.md",
    "release_supply_chain_docs": "docs/release-supply-chain.md"
  },
  "external_release_blockers": [
    "FreeBSD native release lane requires a documented native freebsd-x64 Buildkite queue or a separately proven cross-build lane.",
    "Production release publication requires final release metadata with non-placeholder SHA-256 checksums for every published artifact.",
    "Signing, notarization, SBOM publication, and provenance publication require configured external credentials and policy approval."
  ]
}
EOF

  printf 'completion certificate: %s\n' "${markdown}"
  printf 'completion certificate json: %s\n' "${json}"
}

cd "${CTX_REPO_ROOT}"
CTX_ARTIFACT_DIR="${CTX_ARTIFACT_DIR:-target/ctx-artifacts/completion-certificate}"
ctx_timing_init
trap ctx_timing_finish EXIT
ctx_run_timed "release-completion-certificate" write_certificate "${CTX_ARTIFACT_DIR}"
