#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/ci-common.sh
source "${script_dir}/ci-common.sh"

usage() {
  cat <<'USAGE'
usage: scripts/release-supply-chain-proof.sh [--contract-fixture] [OUT_DIR]

Writes non-publishing dependency advisory/license and supply-chain artifact
evidence. The default mode inventories Cargo.lock and license metadata. It does
not claim that advisory, SBOM, provenance, signing, or notarization checks ran
unless the corresponding external evidence or tools are explicitly supplied.

Environment:
  CTX_RELEASE_RUN_CARGO_AUDIT=1       Run `cargo audit --json` and require PASS.
  CTX_RELEASE_ADVISORY_EVIDENCE_PATH  Manager-approved advisory audit evidence.
  CTX_RELEASE_SBOM_PATH               Generated SBOM evidence file.
  CTX_RELEASE_PROVENANCE_PATH         Generated provenance evidence file.
  CTX_RELEASE_SIGNATURE_BUNDLE_PATH   Signature or signing bundle evidence file.
  CTX_RELEASE_NOTARIZATION_PATH       Notarization evidence file.

Contract fixture evidence is for release contract self-tests only.
USAGE
}

sha256_file() {
  local path="$1"

  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${path}" | awk '{ print $1 }'
    return 0
  fi
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${path}" | awk '{ print $1 }'
    return 0
  fi
  if command -v sha256 >/dev/null 2>&1; then
    sha256 -q "${path}"
    return 0
  fi

  printf 'sha256sum, shasum, or sha256 is required\n' >&2
  return 1
}

require_file() {
  local path="$1"
  local description="$2"

  if [[ ! -s "${path}" ]]; then
    printf '%s is missing or empty: %s\n' "${description}" "${path}" >&2
    return 1
  fi
}

tool_available_json() {
  local tool="$1"

  if command -v "${tool}" >/dev/null 2>&1; then
    printf 'true'
  else
    printf 'false'
  fi
}

cargo_audit_available_json() {
  if cargo audit --version >/dev/null 2>&1; then
    printf 'true'
  else
    printf 'false'
  fi
}

write_contract_fixture() {
  local out_dir="$1"
  local dependency_json artifact_json markdown generated_at commit branch

  mkdir -p "${out_dir}"
  dependency_json="${out_dir}/dependency-advisory-license-audit.json"
  artifact_json="${out_dir}/sbom-provenance-signature.json"
  markdown="${out_dir}/supply-chain-proof.md"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"

  cat > "${dependency_json}" <<EOF
{
  "schema_version": 1,
  "kind": "ctx_dependency_advisory_license_audit",
  "evidence_class": "contract_fixture",
  "self_test_fixture": true,
  "mode": "dependency-advisory-license-audit",
  "status": "blocked_manual_required",
  "publishing": false,
  "package": "ctx",
  "version": "0.1.0",
  "required_before_public_release": true,
  "cargo_lock": {
    "path": "Cargo.lock",
    "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
  },
  "cargo_metadata": {
    "path": "cargo-metadata.json",
    "sha256": "0000000000000000000000000000000000000000000000000000000000000000",
    "packages_checked": 1,
    "workspace_members": 1
  },
  "advisory_audit": {
    "status": "blocked_manual_required",
    "tool": "cargo audit",
    "tool_available": false,
    "manual_lane": true,
    "manager_approval_required": true
  },
  "license_audit": {
    "status": "passed",
    "packages_checked": 1,
    "missing_license_count": 0,
    "manual_policy_review_required": true
  },
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at}
}
EOF

  cat > "${artifact_json}" <<EOF
{
  "schema_version": 1,
  "kind": "ctx_sbom_provenance_signature_evidence",
  "evidence_class": "contract_fixture",
  "self_test_fixture": true,
  "mode": "sbom-provenance-signature",
  "status": "blocked_manual_required",
  "publishing": false,
  "package": "ctx",
  "version": "0.1.0",
  "required_before_public_release": true,
  "manager_approval_required": true,
  "manual_lane": true,
  "sbom": {
    "status": "blocked_manual_required",
    "required_before_public_release": true,
    "manual_lane": true,
    "manager_approval_required": true
  },
  "provenance": {
    "status": "blocked_manual_required",
    "required_before_public_release": true,
    "manual_lane": true,
    "manager_approval_required": true
  },
  "signature": {
    "status": "blocked_manual_required",
    "required_before_public_release": true,
    "manual_lane": true,
    "manager_approval_required": true
  },
  "notarization": {
    "status": "blocked_manual_required",
    "required_before_public_release": true,
    "manual_lane": true,
    "manager_approval_required": true
  },
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at}
}
EOF

  cat > "${markdown}" <<'EOF'
# Supply Chain Proof Contract Fixture

- Evidence class: contract_fixture
- Self-test fixture: true
- Publishing: false
- Advisory, SBOM, provenance, signature, and notarization evidence remain blocked manual lanes.
EOF

  printf 'supply-chain dependency fixture: %s\n' "${dependency_json}"
  printf 'supply-chain artifact fixture: %s\n' "${artifact_json}"
}

evidence_status() {
  local path="$1"

  if [[ -n "${path}" && -s "${path}" ]]; then
    printf 'provided_manual_evidence'
  else
    printf 'blocked_manual_required'
  fi
}

evidence_sha256_or_empty() {
  local path="$1"

  if [[ -n "${path}" && -s "${path}" ]]; then
    sha256_file "${path}"
  else
    printf ''
  fi
}

run_optional_cargo_audit() {
  local out_dir="$1"
  local audit_json="${out_dir}/cargo-audit.json"
  local audit_log="${out_dir}/cargo-audit.stderr.txt"
  local status

  if [[ "${CTX_RELEASE_RUN_CARGO_AUDIT:-0}" != "1" ]]; then
    return 2
  fi
  if ! cargo audit --version >/dev/null 2>&1; then
    printf 'CTX_RELEASE_RUN_CARGO_AUDIT=1 requires cargo-audit\n' >&2
    return 127
  fi

  set +e
  cargo audit --json > "${audit_json}" 2> "${audit_log}"
  status=$?
  set -e
  if (( status != 0 )); then
    printf 'cargo audit failed; see %s\n' "${audit_log}" >&2
    return "${status}"
  fi
  return 0
}

write_supply_chain_evidence() {
  local out_dir="$1"
  local metadata_json dependency_json artifact_json markdown cargo_lock_sha metadata_sha generated_at commit branch
  local cargo_audit_status cargo_audit_tool_available cargo_audit_result cargo_audit_artifact cargo_audit_artifact_sha
  local advisory_evidence advisory_evidence_sha
  local sbom_path provenance_path signature_path notarization_path
  local sbom_status provenance_status signature_status notarization_status
  local sbom_sha provenance_sha signature_sha notarization_sha overall_artifact_status

  mkdir -p "${out_dir}"
  metadata_json="${out_dir}/cargo-metadata.json"
  dependency_json="${out_dir}/dependency-advisory-license-audit.json"
  artifact_json="${out_dir}/sbom-provenance-signature.json"
  markdown="${out_dir}/supply-chain-proof.md"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"

  require_file "Cargo.lock" "Cargo.lock" || return 1
  if ! cargo metadata --locked --format-version 1 > "${metadata_json}"; then
    printf 'cargo metadata --locked failed\n' >&2
    return 1
  fi
  cargo_lock_sha="$(sha256_file Cargo.lock)" || return 1
  metadata_sha="$(sha256_file "${metadata_json}")" || return 1

  cargo_audit_tool_available="$(cargo_audit_available_json)"
  cargo_audit_status="blocked_manual_required"
  cargo_audit_artifact=""
  cargo_audit_artifact_sha=""
  cargo_audit_result=2
  if run_optional_cargo_audit "${out_dir}"; then
    cargo_audit_status="passed"
    cargo_audit_artifact="cargo-audit.json"
    cargo_audit_artifact_sha="$(sha256_file "${out_dir}/cargo-audit.json")"
  else
    cargo_audit_result=$?
    if (( cargo_audit_result != 2 )); then
      return "${cargo_audit_result}"
    fi
  fi

  advisory_evidence="${CTX_RELEASE_ADVISORY_EVIDENCE_PATH:-}"
  advisory_evidence_sha="$(evidence_sha256_or_empty "${advisory_evidence}")"
  if [[ "${cargo_audit_status}" != "passed" && -n "${advisory_evidence}" ]]; then
    require_file "${advisory_evidence}" "advisory audit evidence" || return 1
    cargo_audit_status="provided_manual_evidence"
  fi

  sbom_path="${CTX_RELEASE_SBOM_PATH:-}"
  provenance_path="${CTX_RELEASE_PROVENANCE_PATH:-}"
  signature_path="${CTX_RELEASE_SIGNATURE_BUNDLE_PATH:-}"
  notarization_path="${CTX_RELEASE_NOTARIZATION_PATH:-}"
  sbom_status="$(evidence_status "${sbom_path}")"
  provenance_status="$(evidence_status "${provenance_path}")"
  signature_status="$(evidence_status "${signature_path}")"
  notarization_status="$(evidence_status "${notarization_path}")"
  sbom_sha="$(evidence_sha256_or_empty "${sbom_path}")"
  provenance_sha="$(evidence_sha256_or_empty "${provenance_path}")"
  signature_sha="$(evidence_sha256_or_empty "${signature_path}")"
  notarization_sha="$(evidence_sha256_or_empty "${notarization_path}")"

  overall_artifact_status="passed"
  for status in "${sbom_status}" "${provenance_status}" "${signature_status}" "${notarization_status}"; do
    if [[ "${status}" != "provided_manual_evidence" ]]; then
      overall_artifact_status="blocked_manual_required"
    fi
  done

  if ! CTX_SUPPLY_METADATA_JSON="${metadata_json}" \
  CTX_SUPPLY_DEPENDENCY_JSON="${dependency_json}" \
  CTX_SUPPLY_CARGO_LOCK_SHA="${cargo_lock_sha}" \
  CTX_SUPPLY_METADATA_SHA="${metadata_sha}" \
  CTX_SUPPLY_ADVISORY_STATUS="${cargo_audit_status}" \
  CTX_SUPPLY_ADVISORY_TOOL_AVAILABLE="${cargo_audit_tool_available}" \
  CTX_SUPPLY_ADVISORY_ARTIFACT="${cargo_audit_artifact}" \
  CTX_SUPPLY_ADVISORY_ARTIFACT_SHA="${cargo_audit_artifact_sha}" \
  CTX_SUPPLY_ADVISORY_EVIDENCE_PATH="${advisory_evidence}" \
  CTX_SUPPLY_ADVISORY_EVIDENCE_SHA="${advisory_evidence_sha}" \
  CTX_SUPPLY_GIT_COMMIT="${commit}" \
  CTX_SUPPLY_GIT_BRANCH="${branch}" \
  CTX_SUPPLY_GENERATED_AT="${generated_at}" \
  python3 - <<'PY'
import json
import os
from pathlib import Path

metadata_path = Path(os.environ["CTX_SUPPLY_METADATA_JSON"])
metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
packages = metadata.get("packages", [])
missing = [
    {
        "name": package.get("name", ""),
        "version": package.get("version", ""),
        "source": package.get("source") or "workspace",
    }
    for package in packages
    if not (package.get("license") or package.get("license_file"))
]
licenses = sorted(
    {
        package.get("license") or f"license_file:{package.get('license_file')}"
        for package in packages
        if package.get("license") or package.get("license_file")
    }
)
advisory_status = os.environ["CTX_SUPPLY_ADVISORY_STATUS"]
document = {
    "schema_version": 1,
    "kind": "ctx_dependency_advisory_license_audit",
    "mode": "dependency-advisory-license-audit",
    "status": "passed" if advisory_status == "passed" and not missing else "blocked_manual_required",
    "publishing": False,
    "package": "ctx",
    "version": "0.1.0",
    "required_before_public_release": True,
    "cargo_lock": {
        "path": "Cargo.lock",
        "sha256": os.environ["CTX_SUPPLY_CARGO_LOCK_SHA"],
    },
    "cargo_metadata": {
        "path": "cargo-metadata.json",
        "sha256": os.environ["CTX_SUPPLY_METADATA_SHA"],
        "packages_checked": len(packages),
        "workspace_members": len(metadata.get("workspace_members", [])),
    },
    "advisory_audit": {
        "status": advisory_status,
        "tool": "cargo audit",
        "tool_available": os.environ["CTX_SUPPLY_ADVISORY_TOOL_AVAILABLE"] == "true",
        "tool_artifact": os.environ["CTX_SUPPLY_ADVISORY_ARTIFACT"],
        "tool_artifact_sha256": os.environ["CTX_SUPPLY_ADVISORY_ARTIFACT_SHA"],
        "manual_evidence_path": os.environ["CTX_SUPPLY_ADVISORY_EVIDENCE_PATH"],
        "manual_evidence_sha256": os.environ["CTX_SUPPLY_ADVISORY_EVIDENCE_SHA"],
        "manual_lane": advisory_status != "passed",
        "manager_approval_required": advisory_status != "passed",
    },
    "license_audit": {
        "status": "passed" if not missing else "blocked_manual_required",
        "packages_checked": len(packages),
        "missing_license_count": len(missing),
        "missing_license_packages": missing,
        "unique_licenses": licenses,
        "manual_policy_review_required": True,
    },
    "git_commit": os.environ["CTX_SUPPLY_GIT_COMMIT"],
    "git_branch": os.environ["CTX_SUPPLY_GIT_BRANCH"],
    "generated_at_unix_s": int(os.environ["CTX_SUPPLY_GENERATED_AT"]),
}
Path(os.environ["CTX_SUPPLY_DEPENDENCY_JSON"]).write_text(
    json.dumps(document, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)
PY
  then
    printf 'failed to write dependency advisory/license evidence\n' >&2
    return 1
  fi

  if ! CTX_SUPPLY_ARTIFACT_JSON="${artifact_json}" \
  CTX_SUPPLY_ARTIFACT_STATUS="${overall_artifact_status}" \
  CTX_SUPPLY_SBOM_PATH="${sbom_path}" \
  CTX_SUPPLY_SBOM_STATUS="${sbom_status}" \
  CTX_SUPPLY_SBOM_SHA="${sbom_sha}" \
  CTX_SUPPLY_PROVENANCE_PATH="${provenance_path}" \
  CTX_SUPPLY_PROVENANCE_STATUS="${provenance_status}" \
  CTX_SUPPLY_PROVENANCE_SHA="${provenance_sha}" \
  CTX_SUPPLY_SIGNATURE_PATH="${signature_path}" \
  CTX_SUPPLY_SIGNATURE_STATUS="${signature_status}" \
  CTX_SUPPLY_SIGNATURE_SHA="${signature_sha}" \
  CTX_SUPPLY_NOTARIZATION_PATH="${notarization_path}" \
  CTX_SUPPLY_NOTARIZATION_STATUS="${notarization_status}" \
  CTX_SUPPLY_NOTARIZATION_SHA="${notarization_sha}" \
  CTX_SUPPLY_SYFT_AVAILABLE="$(tool_available_json syft)" \
  CTX_SUPPLY_COSIGN_AVAILABLE="$(tool_available_json cosign)" \
  CTX_SUPPLY_GIT_COMMIT="${commit}" \
  CTX_SUPPLY_GIT_BRANCH="${branch}" \
  CTX_SUPPLY_GENERATED_AT="${generated_at}" \
  python3 - <<'PY'
import json
import os
from pathlib import Path

def evidence(name, status_env, path_env, sha_env):
    status = os.environ[status_env]
    return {
        "status": status,
        "path": os.environ[path_env],
        "sha256": os.environ[sha_env],
        "required_before_public_release": True,
        "manual_lane": status != "provided_manual_evidence",
        "manager_approval_required": status != "provided_manual_evidence",
    }

document = {
    "schema_version": 1,
    "kind": "ctx_sbom_provenance_signature_evidence",
    "mode": "sbom-provenance-signature",
    "status": os.environ["CTX_SUPPLY_ARTIFACT_STATUS"],
    "publishing": False,
    "package": "ctx",
    "version": "0.1.0",
    "required_before_public_release": True,
    "manual_lane": os.environ["CTX_SUPPLY_ARTIFACT_STATUS"] != "passed",
    "manager_approval_required": os.environ["CTX_SUPPLY_ARTIFACT_STATUS"] != "passed",
    "tools": {
        "syft_available": os.environ["CTX_SUPPLY_SYFT_AVAILABLE"] == "true",
        "cosign_available": os.environ["CTX_SUPPLY_COSIGN_AVAILABLE"] == "true",
    },
    "sbom": evidence("sbom", "CTX_SUPPLY_SBOM_STATUS", "CTX_SUPPLY_SBOM_PATH", "CTX_SUPPLY_SBOM_SHA"),
    "provenance": evidence("provenance", "CTX_SUPPLY_PROVENANCE_STATUS", "CTX_SUPPLY_PROVENANCE_PATH", "CTX_SUPPLY_PROVENANCE_SHA"),
    "signature": evidence("signature", "CTX_SUPPLY_SIGNATURE_STATUS", "CTX_SUPPLY_SIGNATURE_PATH", "CTX_SUPPLY_SIGNATURE_SHA"),
    "notarization": evidence("notarization", "CTX_SUPPLY_NOTARIZATION_STATUS", "CTX_SUPPLY_NOTARIZATION_PATH", "CTX_SUPPLY_NOTARIZATION_SHA"),
    "git_commit": os.environ["CTX_SUPPLY_GIT_COMMIT"],
    "git_branch": os.environ["CTX_SUPPLY_GIT_BRANCH"],
    "generated_at_unix_s": int(os.environ["CTX_SUPPLY_GENERATED_AT"]),
}
Path(os.environ["CTX_SUPPLY_ARTIFACT_JSON"]).write_text(
    json.dumps(document, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)
PY
  then
    printf 'failed to write SBOM/provenance/signature evidence\n' >&2
    return 1
  fi

  cat > "${markdown}" <<EOF
# Supply Chain Proof

- Publishing: false
- Dependency advisory status: \`${cargo_audit_status}\`
- License inventory: generated from \`cargo metadata --locked\`
- SBOM status: \`${sbom_status}\`
- Provenance status: \`${provenance_status}\`
- Signature status: \`${signature_status}\`
- Notarization status: \`${notarization_status}\`
- Public release remains blocked until required-before-public-release items are \`passed\` or supplied as manager-approved evidence.
EOF

  printf 'dependency advisory/license evidence: %s\n' "${dependency_json}"
  printf 'SBOM/provenance/signature evidence: %s\n' "${artifact_json}"
  printf 'supply-chain notes: %s\n' "${markdown}"
}

main() {
  local mode="collect"
  local out_dir

  case "${1:-}" in
    -h|--help|help)
      usage
      return 0
      ;;
    --contract-fixture)
      mode="contract-fixture"
      shift
      ;;
  esac

  cd "${CTX_REPO_ROOT}"
  out_dir="${1:-${CTX_ARTIFACT_DIR:-target/ctx-artifacts/supply-chain}}"
  ctx_timing_init
  trap ctx_timing_finish EXIT

  if [[ "${mode}" == "contract-fixture" ]]; then
    ctx_run_timed "release-supply-chain-contract-fixture" write_contract_fixture "${out_dir}"
  else
    ctx_run_timed "release-supply-chain-proof" write_supply_chain_evidence "${out_dir}"
  fi
}

main "$@"
