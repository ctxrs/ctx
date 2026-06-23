#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/ci-common.sh
source "${script_dir}/ci-common.sh"

contract_failures=0
contract_summary=""

note() {
  printf '%s\n' "$*" | tee -a "${contract_summary}"
}

fail_contract() {
  contract_failures=$(( contract_failures + 1 ))
  note "FAIL: $*"
}

pass_contract() {
  note "PASS: $*"
}

file_contains() {
  local file="$1"
  local text="$2"

  if command -v rg >/dev/null 2>&1; then
    rg --fixed-strings -q -- "${text}" "${file}"
    return $?
  fi

  grep -F -q -- "${text}" "${file}"
}

require_text() {
  local description="$1"
  local file="$2"
  local text="$3"

  if file_contains "${file}" "${text}"; then
    pass_contract "${description}"
  else
    fail_contract "${description} (${file} missing ${text})"
  fi
}

validate_yaml_if_possible() {
  local pipeline="$1"

  if command -v buildkite-agent >/dev/null 2>&1; then
    if BUILDKITE_AGENT_ACCESS_TOKEN="${BUILDKITE_AGENT_ACCESS_TOKEN:-dry-run}" \
      buildkite-agent pipeline upload \
        --dry-run \
        --format json \
        --no-interpolation \
        --no-color \
        --log-level error \
        "${pipeline}" > "${CTX_ARTIFACT_DIR}/pipeline-dry-run.json"; then
      pass_contract "Buildkite agent dry-run parser accepts pipeline"
    else
      fail_contract "Buildkite agent dry-run parser"
    fi
    if BUILDKITE_AGENT_ACCESS_TOKEN="${BUILDKITE_AGENT_ACCESS_TOKEN:-dry-run}" \
      BUILDKITE_COMMIT="${BUILDKITE_COMMIT:-0000000000000000000000000000000000000000}" \
      TMPDIR="${TMPDIR:-/tmp}" \
      buildkite-agent pipeline upload \
        --dry-run \
        --format json \
        --no-color \
        --log-level error \
        "${pipeline}" > "${CTX_ARTIFACT_DIR}/pipeline-interpolated-dry-run.json"; then
      pass_contract "Buildkite agent interpolation parser accepts pipeline"
    else
      fail_contract "Buildkite agent interpolation parser"
    fi
    return 0
  fi

  if ! command -v ruby >/dev/null 2>&1; then
    note "SKIP: YAML parser check (buildkite-agent and ruby unavailable)"
    return 0
  fi

  if ruby -e '
    require "yaml"
    data = YAML.load_file(ARGV.fetch(0))
    unless data.is_a?(Hash) && data["steps"].is_a?(Array)
      abort "pipeline must be a mapping with a steps array"
    end
    keys = data["steps"].map { |step| step.is_a?(Hash) ? step["key"] : nil }.compact
    duplicates = keys.group_by(&:itself).select { |_key, values| values.size > 1 }.keys
    abort "duplicate step keys: #{duplicates.join(", ")}" unless duplicates.empty?
  ' "${pipeline}"; then
    pass_contract "Buildkite YAML parses and step keys are unique"
  else
    fail_contract "Buildkite YAML parse or duplicate-key check"
  fi
}

validate_contract() {
  local pipeline=".buildkite/pipeline.yml"
  local release_script="scripts/release-dry-run.sh"
  local release_candidate_script="scripts/release-candidate-metadata.sh"
  local provider_live_script="scripts/release-provider-live-e2e-lanes.sh"
  local r2_staging_script="scripts/release-r2-staging-smoke.sh"
  local certificate_script="scripts/release-completion-certificate.sh"
  local install_script="scripts/install.sh"
  local install_ps1="scripts/install.ps1"
  local windows_script="scripts/ci-windows.ps1"
  local blocker_script="scripts/release-platform-blocker.sh"

  test -f "${pipeline}" || fail_contract "pipeline file exists"
  test -f "${release_script}" || fail_contract "release dry-run script exists"
  test -f "${release_candidate_script}" || fail_contract "release candidate metadata script exists"
  test -f "${provider_live_script}" || fail_contract "provider live E2E lane script exists"
  test -f "${r2_staging_script}" || fail_contract "R2 staging smoke script exists"
  test -f "${certificate_script}" || fail_contract "completion certificate script exists"
  test -f "${install_script}" || fail_contract "Bash installer exists"
  test -f "${install_ps1}" || fail_contract "PowerShell installer exists"
  test -f "${windows_script}" || fail_contract "Windows PowerShell CI script exists"
  if [[ -f scripts/ci-windows-bash.cmd ]]; then
    fail_contract "Windows Bash wrapper has been removed"
  fi
  test -f "${blocker_script}" || fail_contract "release platform blocker script exists"

  validate_yaml_if_possible "${pipeline}"

  require_text "pipeline contract step" "${pipeline}" 'key: "pipeline-contract"'
  require_text "Linux fmt step" "${pipeline}" 'key: "fmt"'
  require_text "Linux docs step" "${pipeline}" 'key: "docs"'
  require_text "Linux cargo check step" "${pipeline}" 'key: "cargo-check"'
  require_text "Linux clippy step" "${pipeline}" 'key: "clippy"'
  require_text "Linux cargo test step" "${pipeline}" 'key: "cargo-test"'
  require_text "Linux examples step" "${pipeline}" 'key: "examples"'
  require_text "Linux Bazel step" "${pipeline}" 'key: "bazel"'
  require_text "product decision regression step" "${pipeline}" 'key: "product-decisions"'
  require_text "provider fixture import step" "${pipeline}" 'key: "provider-fixtures"'
  require_text "provider live E2E definitions step" "${pipeline}" 'key: "provider-live-e2e-lanes"'
  require_text "provider live E2E gated step" "${pipeline}" 'key: "provider-live-e2e-gated"'
  require_text "rich search/context step" "${pipeline}" 'key: "rich-search-context"'
  require_text "dashboard/report artifact review step" "${pipeline}" 'key: "dashboard-report-artifact-review"'
  require_text "PR publish dry-run step" "${pipeline}" 'key: "pr-publish-dry-run"'
  require_text "security archive fixtures step" "${pipeline}" 'key: "security-archive-fixtures"'
  require_text "jj e2e blocker status step" "${pipeline}" 'key: "jj-e2e-blocker-status"'
  require_text "installer dry-run smoke step" "${pipeline}" 'key: "installer-dry-run-smoke"'
  require_text "Linux x64 smoke step" "${pipeline}" 'key: "platform-smoke-linux-x64"'
  require_text "macOS arm64 smoke step" "${pipeline}" 'key: "platform-smoke-macos-arm64"'
  require_text "macOS x64 smoke step" "${pipeline}" 'key: "platform-smoke-macos-x64"'
  require_text "Windows x64 smoke step" "${pipeline}" 'key: "platform-smoke-windows-x64"'
  require_text "Linux release dry-run step" "${pipeline}" 'key: "release-dry-run-linux-x64"'
  require_text "macOS arm64 release dry-run step" "${pipeline}" 'key: "release-dry-run-macos-arm64"'
  require_text "macOS x64 release dry-run step" "${pipeline}" 'key: "release-dry-run-macos-x64"'
  require_text "Windows x64 release dry-run step" "${pipeline}" 'key: "release-dry-run-windows-x64"'
  require_text "FreeBSD documented blocker step" "${pipeline}" 'key: "freebsd-x64-blocker"'
  require_text "release candidate metadata step" "${pipeline}" 'key: "release-candidate-metadata"'
  require_text "R2 staging smoke step" "${pipeline}" 'key: "r2-staging-smoke"'
  require_text "completion certificate step" "${pipeline}" 'key: "release-completion-certificate"'
  require_text "Linux platform smoke waits for finished-product lanes" "${pipeline}" 'depends_on: "installer-dry-run-smoke"'
  require_text "provider fixtures wait for product decisions" "${pipeline}" 'depends_on: "product-decisions"'
  require_text "FreeBSD blocker waits for Linux release dry-run" "${pipeline}" 'depends_on: "release-dry-run-linux-x64"'
  require_text "rich search waits for provider live E2E definitions" "${pipeline}" 'depends_on: "provider-live-e2e-lanes"'

  require_text "Linux verification queue" "${pipeline}" 'queue: "release-linux-managed"'
  require_text "Linux verification runner class" "${pipeline}" 'ctx-runner-class: "release-linux-x64-stage"'
  require_text "Linux release queue" "${pipeline}" 'queue: "release-linux-managed"'
  require_text "Linux release runner class" "${pipeline}" 'ctx-runner-class: "release-linux-x64-stage"'
  require_text "macOS arm64 queue" "${pipeline}" 'queue: "ctx-mac-gui-shared-arm64"'
  require_text "macOS x64 queue" "${pipeline}" 'queue: "ctx-mac-gui-shared-x64"'
  require_text "Windows x64 queue" "${pipeline}" 'queue: "windows-x64"'
  require_text "Windows PowerShell wrapper" "${pipeline}" 'powershell -NoProfile -ExecutionPolicy Bypass -File scripts\\ci-windows.ps1'
  require_text "macOS custom checkout plugin" "${pipeline}" 'custom-checkout#v1.8.0'
  require_text "macOS custom checkout cleanup" "${pipeline}" 'delete_checkout: true'
  require_text "macOS isolated checkout root" "${pipeline}" 'interpolate_checkout_path: "$${TMPDIR:-/tmp}/ctx-records-$${BUILDKITE_BUILD_NUMBER}-$${BUILDKITE_STEP_KEY}-$${BUILDKITE_JOB_ID}"'
  require_text "macOS hook compatibility script exists" "scripts/buildkite/macos_agent_pre_command.sh" 'Compatibility hook for shared macOS Buildkite agents.'

  require_text "docs command wired" "${pipeline}" './scripts/check.sh docs'
  require_text "examples command wired" "${pipeline}" './scripts/check.sh examples'
  require_text "Bazel is required in CI" "${pipeline}" 'CTX_REQUIRE_BAZEL=1 ./scripts/check.sh bazel'
  require_text "provider fixtures command wired" "${pipeline}" './scripts/check.sh provider-fixtures'
  require_text "product decisions command wired" "${pipeline}" './scripts/check.sh product-decisions'
  require_text "provider live E2E definitions command wired" "${pipeline}" './scripts/release-provider-live-e2e-lanes.sh definitions'
  require_text "provider live E2E gated command wired" "${pipeline}" './scripts/release-provider-live-e2e-lanes.sh run-selected'
  require_text "provider live E2E global opt-in" "${provider_live_script}" 'CTX_LIVE_PROVIDER_E2E=1'
  require_text "provider live E2E requires provider opt-in" "${provider_live_script}" 'CTX_LIVE_PROVIDER_<PROVIDER>=1'
  require_text "rich search/context command wired" "${pipeline}" './scripts/check.sh rich-search-context'
  require_text "dashboard/report command wired" "${pipeline}" './scripts/check.sh dashboard-report-artifact-review'
  require_text "PR publish dry-run command wired" "${pipeline}" './scripts/check.sh pr-publish-dry-run'
  require_text "security archive fixtures command wired" "${pipeline}" './scripts/check.sh security-archive-fixtures'
  require_text "jj e2e blocker command wired" "${pipeline}" './scripts/check.sh jj-e2e-blocker-status'
  require_text "installer dry-run smoke command wired" "${pipeline}" './scripts/check.sh installer-dry-run-smoke'
  require_text "platform smoke command wired" "${pipeline}" './scripts/check.sh platform-smoke'
  require_text "Windows platform smoke command wired" "${pipeline}" 'scripts\\ci-windows.ps1 platform-smoke'
  require_text "Windows release dry-run command wired" "${pipeline}" 'scripts\\ci-windows.ps1 release-dry-run'
  require_text "release candidate metadata command wired" "${pipeline}" './scripts/release-candidate-metadata.sh'
  require_text "R2 staging smoke command wired" "${pipeline}" './scripts/release-r2-staging-smoke.sh'
  require_text "completion certificate command wired" "${pipeline}" './scripts/release-completion-certificate.sh'
  require_text "completion certificate uses tolerant artifact download helper" "${pipeline}" 'download_artifacts()'
  require_text "completion certificate normalizes Windows artifact paths" "${pipeline}" 'normalize_downloaded_artifact_paths()'
  require_text "completion certificate escapes runtime shell variables for Buildkite interpolation" "${pipeline}" 'rel="$${file#./}"'
  require_text "completion certificate downloads backslash artifact paths" "${pipeline}" 'buildkite-agent artifact download "$${windows_prefix}\\*" . || true'
  require_text "completion certificate downloads pipeline contract artifact" "${pipeline}" 'download_artifacts "artifacts/buildkite/pipeline-contract"'
  require_text "completion certificate downloads Linux release dry-run artifacts" "${pipeline}" 'download_artifacts "artifacts/buildkite/release-dry-run/linux-x64"'
  require_text "completion certificate downloads macOS arm64 release dry-run artifacts" "${pipeline}" 'download_artifacts "artifacts/buildkite/release-dry-run/macos-arm64"'
  require_text "completion certificate downloads macOS x64 release dry-run artifacts" "${pipeline}" 'download_artifacts "artifacts/buildkite/release-dry-run/macos-x64"'
  require_text "completion certificate downloads release dry-run artifacts" "${pipeline}" 'download_artifacts "artifacts/buildkite/release-dry-run/windows-x64"'
  require_text "completion certificate downloads FreeBSD blocker artifact" "${pipeline}" 'download_artifacts "artifacts/buildkite/release-blockers/freebsd-x64"'
  require_text "completion certificate downloads release candidate artifacts" "${pipeline}" 'download_artifacts "artifacts/buildkite/release-candidate"'
  require_text "completion certificate downloads R2 staging smoke artifact" "${pipeline}" 'download_artifacts "artifacts/buildkite/r2-staging-smoke"'
  require_text "completion certificate downloads product decision artifact" "${pipeline}" 'download_artifacts "artifacts/buildkite/finished-product/product-decisions"'
  require_text "completion certificate downloads provider fixture artifact" "${pipeline}" 'download_artifacts "artifacts/buildkite/finished-product/provider-fixtures"'
  require_text "completion certificate downloads provider live E2E lane artifact" "${pipeline}" 'download_artifacts "artifacts/buildkite/provider-live-e2e-lanes"'
  require_text "completion certificate downloads rich search/context artifact" "${pipeline}" 'download_artifacts "artifacts/buildkite/finished-product/rich-search-context"'
  require_text "completion certificate downloads dashboard/report artifact" "${pipeline}" 'download_artifacts "artifacts/buildkite/finished-product/dashboard-report-artifact-review"'
  require_text "completion certificate downloads PR publish dry-run artifact" "${pipeline}" 'download_artifacts "artifacts/buildkite/finished-product/pr-publish-dry-run"'
  require_text "completion certificate downloads security archive fixture artifact" "${pipeline}" 'download_artifacts "artifacts/buildkite/finished-product/security-archive-fixtures"'
  require_text "completion certificate downloads jj blocker artifact" "${pipeline}" 'download_artifacts "artifacts/buildkite/finished-product/jj-e2e-blocker-status"'
  require_text "completion certificate downloads finished-product artifacts" "${pipeline}" 'download_artifacts "artifacts/buildkite/finished-product/installer-dry-run-smoke"'
  require_text "artifact upload includes root pipeline contract files" "${pipeline}" '- "artifacts/buildkite/pipeline-contract/*"'
  require_text "artifact upload includes nested dashboard files" "${pipeline}" '- "artifacts/buildkite/finished-product/dashboard-report-artifact-review/**/*"'
  require_text "completion certificate waits for Linux release dry-run" "${pipeline}" '- "release-dry-run-linux-x64"'
  require_text "completion certificate waits for macOS arm64 release dry-run" "${pipeline}" '- "release-dry-run-macos-arm64"'
  require_text "completion certificate waits for macOS x64 release dry-run" "${pipeline}" '- "release-dry-run-macos-x64"'
  require_text "completion certificate waits for Windows release dry-run" "${pipeline}" '- "release-dry-run-windows-x64"'
  require_text "completion certificate waits for release candidate metadata" "${pipeline}" '- "release-candidate-metadata"'
  require_text "completion certificate waits for R2 staging smoke" "${pipeline}" '- "r2-staging-smoke"'
  require_text "completion certificate waits for product decisions" "${pipeline}" '- "product-decisions"'
  require_text "completion certificate waits for provider live E2E definitions" "${pipeline}" '- "provider-live-e2e-lanes"'
  require_text "completion certificate waits for installer dry-run smoke" "${pipeline}" '- "installer-dry-run-smoke"'
  require_text "Linux host triple guard" "${pipeline}" 'CTX_EXPECT_HOST_TRIPLE: "x86_64-unknown-linux-gnu"'
  require_text "macOS arm64 host triple guard" "${pipeline}" 'CTX_EXPECT_HOST_TRIPLE: "aarch64-apple-darwin"'
  require_text "macOS x64 host triple guard" "${pipeline}" 'CTX_EXPECT_HOST_TRIPLE: "x86_64-apple-darwin"'
  require_text "Windows x64 host triple guard" "${pipeline}" 'CTX_EXPECT_HOST_TRIPLE: "x86_64-pc-windows-gnu"'
  require_text "Windows x64 release target" "${pipeline}" 'CTX_RELEASE_TARGET_TRIPLE: "x86_64-pc-windows-gnu"'
  require_text "FreeBSD target recorded" "${pipeline}" 'x86_64-unknown-freebsd'

  require_text "release script is dry-run only" "${release_script}" '"dry_run": true'
  require_text "release script does not publish" "${release_script}" '"upload": false'
  require_text "release script enforces host triple" "${release_script}" 'ctx_require_host_triple "${CTX_EXPECT_HOST_TRIPLE:-}"'
  require_text "release script emits checksum file" "${release_script}" 'checksums.sha256'
  require_text "release script manifest records artifact path" "${release_script}" '"path": "$(ctx_json_escape "${artifact_rel}")"'
  require_text "release script manifest records artifact checksum" "${release_script}" '"sha256": "$(ctx_json_escape "${checksum}")"'
  require_text "release script manifest records artifact size" "${release_script}" '"bytes": ${bytes}'
  require_text "release script emits install metadata" "${release_script}" 'ctx-release-metadata.env'
  require_text "release script emits pinned installer checksum" "${release_script}" 'CTX_RELEASE_SHA256_${platform_key}=${checksum}'
  require_text "release candidate metadata is non-publishing" "${release_candidate_script}" '"publishing": false'
  require_text "release candidate metadata is staging-plan-only" "${release_candidate_script}" '"release_candidate_status": "staging_plan_only"'
  require_text "release candidate metadata is launch blocked" "${release_candidate_script}" '"launch_ready": false'
  require_text "release candidate metadata records no R2 upload" "${release_candidate_script}" '"upload_performed": false'
  require_text "release candidate metadata requires 0.1.0" "${release_candidate_script}" 'release candidate metadata expects ctx 0.1.0'
  require_text "release candidate metadata emits combined installer metadata" "${release_candidate_script}" 'ctx-release-metadata.env'
  require_text "release candidate metadata emits R2 upload plan" "${release_candidate_script}" 'r2-upload-plan.md'
  require_text "release candidate metadata emits cleanup commands" "${release_candidate_script}" 'Cleanup staged objects'
  require_text "release candidate metadata uses Wrangler R2 put" "${release_candidate_script}" 'wrangler r2 object put'
  require_text "release candidate metadata stages Bash installer" "${release_candidate_script}" 'scripts/install.sh'
  require_text "release candidate metadata stages PowerShell installer" "${release_candidate_script}" 'scripts/install.ps1'
  require_text "provider live E2E definitions are non-publishing" "${provider_live_script}" '"publishing": false'
  require_text "provider live E2E definitions use opt-in env" "${provider_live_script}" 'CTX_LIVE_PROVIDER_E2E=1'
  require_text "provider live E2E blockers require explicit acceptance" "${provider_live_script}" 'CTX_LIVE_PROVIDER_E2E_ACCEPT_BLOCKERS'
  require_text "R2 staging smoke is non-publishing" "${r2_staging_script}" '"publishing": false'
  require_text "R2 staging smoke records no upload" "${r2_staging_script}" '"upload_performed": false'
  require_text "R2 staging smoke blocks ctx.rs cutover" "${r2_staging_script}" '"no_ctx_rs_cutover": true'
  require_text "R2 staging smoke rejects stale release naming" "${r2_staging_script}" 'stale public product naming'
  require_text "Bash installer requires metadata" "${install_script}" '--metadata is required'
  require_text "Bash installer refuses curl pipe ambiguity" "${install_script}" 'curl -fsSLO'
  require_text "Bash installer refuses insecure metadata URL" "${install_script}" 'refusing insecure metadata URL'
  require_text "Bash installer verifies SHA-256 before install" "${install_script}" 'checksum mismatch'
  require_text "Bash installer rejects placeholder checksums" "${install_script}" 'checksum for ${platform} is a placeholder'
  require_text "PowerShell installer requires metadata" "${install_ps1}" '[Parameter(Mandatory = $true)]'
  require_text "PowerShell installer refuses insecure metadata URL" "${install_ps1}" 'refusing insecure metadata URL'
  require_text "PowerShell installer verifies SHA-256 before install" "${install_ps1}" 'checksum mismatch'
  require_text "PowerShell installer rejects placeholder checksums" "${install_ps1}" 'checksum for windows-x64 is a placeholder'
  require_text "install metadata template exists" "release/install/ctx-release-metadata.env.template" 'CTX_RELEASE_SCHEMA_VERSION=1'
  require_text "install metadata template records 0.1.0" "release/install/ctx-release-metadata.env.template" 'CTX_RELEASE_VERSION=0.1.0'
  require_text "install metadata template records R2 prefix" "release/install/ctx-release-metadata.env.template" 'CTX_RELEASE_R2_PREFIX=ctx/records/release-candidate/v0.1.0/<git-commit>'
  require_text "install metadata template records Bash installer object" "release/install/ctx-release-metadata.env.template" 'CTX_RELEASE_INSTALLER_SH_R2_OBJECT='
  require_text "install metadata template records PowerShell installer object" "release/install/ctx-release-metadata.env.template" 'CTX_RELEASE_INSTALLER_PS1_R2_OBJECT='
  require_text "install docs avoid pipe launch" "docs/release-install.md" 'Do not document or publish a `curl ... | sh` command.'
  require_text "supply chain docs cover SBOM" "docs/release-supply-chain.md" 'SBOM publication is a release blocker'
  require_text "supply chain docs cover provenance" "docs/release-supply-chain.md" 'Build provenance is a release blocker'
  require_text "supply chain docs cover notarization" "docs/release-supply-chain.md" 'notarization'
  require_text "completion certificate is non-publishing" "${certificate_script}" '"publishing": false'
  require_text "completion certificate is launch blocked" "${certificate_script}" '"launch_ready": false'
  require_text "completion certificate is not release approval" "${certificate_script}" '"release_approval": false'
  require_text "completion certificate does not mark evidence verified as real RC" "${certificate_script}" '"evidence_verified": false'
  require_text "completion certificate marks scaffold evidence separately" "${certificate_script}" '"evidence_scaffold_verified": true'
  require_text "completion certificate records external blockers" "${certificate_script}" 'external_release_blockers'
  require_text "completion certificate records provider fixture artifact" "${certificate_script}" 'provider_fixture_import'
  require_text "completion certificate records release candidate metadata" "${certificate_script}" 'release_candidate_metadata'
  require_text "completion certificate records R2 staging smoke" "${certificate_script}" 'r2_staging_smoke'
  require_text "completion certificate records product decision regressions" "${certificate_script}" 'product_decision_regressions'
  require_text "completion certificate records provider live E2E definitions" "${certificate_script}" 'provider_live_e2e_lane_definitions'
  require_text "completion certificate records rich search/context artifact" "${certificate_script}" 'rich_search_context'
  require_text "completion certificate records dashboard/report artifact" "${certificate_script}" 'dashboard_report_artifact_review'
  require_text "completion certificate records PR publish dry-run artifact" "${certificate_script}" 'pr_publish_dry_run'
  require_text "completion certificate records security archive fixture artifact" "${certificate_script}" 'security_archive_fixtures'
  require_text "completion certificate records jj blocker artifact" "${certificate_script}" 'jj_e2e_blocker_status'
  require_text "completion certificate records installer dry-run artifact" "${certificate_script}" 'installer_dry_run_smoke'
  require_text "completion certificate template exists" "release/completion-certificate-template.md" 'ctx Completion Certificate'
  require_text "completion certificate template includes finished-product evidence" "release/completion-certificate-template.md" 'Provider fixture import artifact'
  require_text "completion certificate template includes R2 evidence" "release/completion-certificate-template.md" 'Release candidate R2 upload plan'
  require_text "completion certificate template is launch blocked" "release/completion-certificate-template.md" 'Launch ready'
  require_text "release R2 layout docs exist" "docs/release-r2-layout.md" 'Release R2 Layout'
  require_text "FreeBSD worker notes exist" "docs/freebsd-release-worker.md" 'FreeBSD Release Worker Notes'
  require_text "release R2 machine layout exists" "release/r2-artifact-layout.json" '"kind": "r2_artifact_layout"'
  require_text "release R2 machine layout uses ctx records prefix" "release/r2-artifact-layout.json" '"prefix_template": "ctx/records/release-candidate/v0.1.0/<git-commit>"'
  require_text "check script supports product decision mode" "scripts/check.sh" 'product-decisions)'
  require_text "check script supports provider fixtures" "scripts/check.sh" 'provider-fixtures)'
  require_text "check script supports rich search/context" "scripts/check.sh" 'rich-search-context)'
  require_text "check script supports dashboard/report review" "scripts/check.sh" 'dashboard-report-artifact-review)'
  require_text "check script supports PR publish dry-run" "scripts/check.sh" 'pr-publish-dry-run)'
  require_text "check script supports security archive fixtures" "scripts/check.sh" 'security-archive-fixtures)'
  require_text "check script supports jj blocker status" "scripts/check.sh" 'jj-e2e-blocker-status)'
  require_text "check script supports installer dry-run smoke" "scripts/check.sh" 'installer-dry-run-smoke)'
  require_text "check script supports completion certificate mode" "scripts/check.sh" 'completion-certificate)'
  require_text "provider fixtures are inert" "tests/fixtures/provider/codex.jsonl" '"source":"fixture"'
  require_text "security archive fixtures are referenced" "scripts/check.sh" 'import_rejects_hostile_archive_blob_path_and_rolls_back'
  require_text "PR publish lane is dry-run only" "scripts/check.sh" 'publish pr-comment "${record_id}" --dry-run'
  require_text "installer smoke is dry-run only" "scripts/check.sh" 'scripts/install.sh --metadata "${metadata}" --platform linux-x64 --bin-dir "${CTX_ARTIFACT_DIR}/bin" --dry-run'
  require_text "supply chain docs cover finished-product matrix" "docs/release-supply-chain.md" 'Finished-Product Evidence Matrix'
  require_text "supply chain docs cover PR publish dry-run" "docs/release-supply-chain.md" 'PR publish dry-run'
  require_text "install docs cover installer dry-run smoke" "docs/release-install.md" 'installer dry-run smoke lane'
  require_text "install docs cover R2 audit keys" "docs/release-install.md" 'CTX_RELEASE_R2_OBJECT_<platform>'
  require_text "install docs cover installer object keys" "docs/release-install.md" 'CTX_RELEASE_INSTALLER_SH_R2_OBJECT'
  require_text "supply chain docs cover provider live E2E" "docs/release-supply-chain.md" 'Provider Live E2E'
  require_text "supply chain docs cover R2 staging layout" "docs/release-supply-chain.md" 'R2 Staging Layout'
  require_text "supply chain docs says certificate is launch blocked" "docs/release-supply-chain.md" 'launch_ready: false'
  require_text "host triple parser is pipe-safe" "scripts/ci-common.sh" 'rustc_info="$(rustc -vV)"'
  require_text "rustup bootstrap avoids pipefail SIGPIPE" "scripts/ci-common.sh" '-o "${rustup_installer}"'
  require_text "Windows script bootstraps Rust" "${windows_script}" 'Ensure-Rust-Toolchain'
  require_text "Windows release emits install metadata" "${windows_script}" 'ctx-release-metadata.env'
  require_text "Windows release metadata channel is dry-run" "${windows_script}" 'CTX_RELEASE_CHANNEL=dry-run'
  require_text "Windows release emits pinned installer checksum" "${windows_script}" 'CTX_RELEASE_SHA256_$platformKey=$checksum'
  require_text "Windows release manifest uses safe relative artifact path" "${windows_script}" 'path = $artifactManifestPath'
  require_text "Windows release manifest normalizes artifact slashes" "${windows_script}" '$artifactManifestPath = $artifactManifestPath -replace "\\", "/"'
  require_text "Windows release manifest rejects rooted artifact path" "${windows_script}" '[System.IO.Path]::IsPathRooted($artifactManifestPath)'
  require_text "Windows script initializes MSVC environment" "${windows_script}" 'Ensure-MSVC-Build-Environment'
  require_text "Windows script discovers Visual Studio tools" "${windows_script}" 'vswhere.exe'
  require_text "Windows script bootstraps MinGW GNU tools" "${windows_script}" 'Ensure-MinGW-GNU-Build-Environment'
  require_text "Windows script downloads w64devkit" "${windows_script}" 'skeeto/w64devkit/releases/download'
  require_text "Windows script extracts w64devkit with 7zr" "${windows_script}" 'www.7-zip.org/a/7zr.exe'
  require_text "Windows script provisions libgcc_eh compatibility" "${windows_script}" 'libgcc_eh.a'
  require_text "Windows script uses MinGW linker" "${windows_script}" 'CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER'
  require_text "Windows script uses external Buildkite tool cache" "${windows_script}" 'BUILDKITE_AGENT_HOME'
  require_text "Windows script can bootstrap Visual Studio Build Tools" "${windows_script}" 'Install-Visual-Studio-Build-Tools'
  require_text "Windows script supports platform smoke" "${windows_script}" 'platform-smoke'
  require_text "Windows script supports release dry-run" "${windows_script}" 'release-dry-run'
  require_text "Windows script avoids automatic Args parameter" "${windows_script}" 'param([string[]]$CargoArgs)'
  require_text "Windows script parses typed record JSON" "${windows_script}" '$recordJson.record.id'
  require_text "Windows script handles detached branch metadata" "${windows_script}" 'BUILDKITE_BRANCH'
  require_text "Windows script uses named Cargo args for platform smoke" "${windows_script}" 'Run-Cargo -CargoArgs (@("build", "-p", "ctx", "--bin", "ctx") + $locked)'
  require_text "Windows script uses named Cargo args for release dry-run" "${windows_script}" 'Run-Cargo -CargoArgs (@("build", "--workspace", "--release", "--bins") + $locked)'
  require_text "FreeBSD blocker marks publishing false" "${blocker_script}" '"publishing": false'
  require_text "completion certificate validates release dry-run manifests" "${certificate_script}" 'validate_release_dry_run'
  require_text "completion certificate parses release manifests" "${certificate_script}" 'manifest_value'
  require_text "completion certificate requires checksum files" "${certificate_script}" 'checksums.sha256'
  require_text "completion certificate verifies manifest checksum against metadata" "${certificate_script}" 'metadata checksum must equal manifest artifact checksum'
  require_text "completion certificate verifies artifact file checksum" "${certificate_script}" 'artifact file checksum must equal manifest checksum'
  require_text "completion certificate tolerates Windows metadata CRLF" "${certificate_script}" 'sub(/\r$/, "", value)'
  require_text "completion certificate tolerates Windows checksum CRLF" "${certificate_script}" 'sub(/\r$/, "", artifact)'
  require_text "completion certificate binds release evidence to current head" "${certificate_script}" 'git_commit must match current HEAD'
  require_text "completion certificate rejects untrusted self-test release fixtures" "${certificate_script}" 'self-test fixture and cannot satisfy real completion evidence'
  require_text "check all includes explicit completion certificate self-test lane" "scripts/check.sh" 'run_mode completion-certificate-self-test'
  require_text "check script supports real completion certificate mode" "scripts/check.sh" 'completion-certificate)'
  require_text "check script supports explicit completion certificate self-test mode" "scripts/check.sh" 'completion-certificate-self-test)'
  require_text "completion certificate self-test fixture writes checksum files" "scripts/check.sh" 'checksums.sha256'
  require_text "completion certificate self-test fixture writes artifact files" "scripts/check.sh" 'ctx dry-run artifact self-test fixture'
  require_text "completion certificate self-test covers Windows CRLF release metadata" "scripts/check.sh" 'line_end=$'\''\r\n'\'''
  require_text "completion certificate negative coverage includes checksum mismatch" "scripts/check.sh" 'checksum-mismatch|artifact file checksum must equal manifest checksum'
  require_text "completion certificate negative coverage includes metadata mismatch" "scripts/check.sh" 'metadata-mismatch|metadata checksum must equal manifest artifact checksum'
  require_text "completion certificate negative coverage includes missing checksum file" "scripts/check.sh" 'missing-checksum-file|required evidence is missing or empty: artifacts/buildkite/release-dry-run/linux-x64/checksums.sha256'
  require_text "completion certificate negative coverage includes unsafe artifact path" "scripts/check.sh" 'unsafe-artifact-path|manifest must record a safe relative artifact path'
  require_text "completion certificate negative coverage includes bad artifact count" "scripts/check.sh" 'bad-artifact-count|manifest must record exactly one release artifact'
  require_text "completion certificate negative coverage includes failing summary" "scripts/check.sh" 'failing-finished-product-summary|provider-fixtures summary records passing status'
  require_text "completion certificate negative coverage includes stale release commit" "scripts/check.sh" 'stale-release-commit|git_commit must match current HEAD'
  require_text "completion certificate self-test fixtures are marked synthetic" "scripts/check.sh" '"self_test_fixture": true'
  require_text "completion certificate self-test requires explicit fixture allow" "scripts/check.sh" 'CTX_COMPLETION_CERTIFICATE_ALLOW_SELF_TEST_FIXTURES=1'
  require_text "completion certificate real local mode runs prerequisites" "scripts/check.sh" 'run_completion_certificate_prerequisites "${root}"'
  require_text "completion certificate requires dry-run release metadata" "${certificate_script}" 'CTX_RELEASE_CHANNEL" "dry-run"'
  require_text "completion certificate requires finished-product status summaries" "${certificate_script}" 'require_summary_status'
  require_text "completion certificate verifies evidence before writing" "${certificate_script}" 'validate_evidence'
  require_text "completion certificate validates release candidate metadata" "${certificate_script}" 'validate_release_candidate_metadata'
  require_text "completion certificate validates R2 staging smoke" "${certificate_script}" 'r2-staging-smoke/r2-staging-smoke.json'
  require_text "completion certificate enforces ctx records R2 prefix" "${certificate_script}" 'release candidate R2 prefix must use ctx/records public artifact layout'
  require_text "completion certificate rejects work-record public URLs" "${certificate_script}" 'release candidate public base URL must not brand public artifact URLs around work-record'
  require_text "completion certificate validates provider live E2E lane definitions" "${certificate_script}" 'validate_provider_live_e2e_lanes'
  require_text "completion certificate requires product decision evidence" "${certificate_script}" 'product-decisions/product-decisions.json'

  if (( contract_failures > 0 )); then
    note "Buildkite pipeline contract failed with ${contract_failures} issue(s)."
    return 1
  fi

  note "Buildkite pipeline contract ok."
}

cd "${CTX_REPO_ROOT}"
CTX_ARTIFACT_DIR="${CTX_ARTIFACT_DIR:-target/ctx-artifacts/buildkite-contract}"
mkdir -p "${CTX_ARTIFACT_DIR}"
contract_summary="${CTX_ARTIFACT_DIR}/pipeline-contract.txt"
: > "${contract_summary}"
ctx_timing_init
trap ctx_timing_finish EXIT
ctx_run_timed "buildkite-pipeline-contract" validate_contract
