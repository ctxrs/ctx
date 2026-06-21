#!/usr/bin/env bash
set -euo pipefail

resolve_repo_root() {
  local candidate=""
  for candidate in \
    "${RUNFILES_DIR:-}/_main" \
    "${RUNFILES_DIR:-}/${TEST_WORKSPACE:-}" \
    "${BUILD_WORKSPACE_DIRECTORY:-}" \
    "${PWD:-}"
  do
    if [[ -n "${candidate}" && -f "${candidate}/.buildkite/pipeline.yml" ]]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done
  return 1
}

REPO_ROOT="$(resolve_repo_root)"
PIPELINE="${REPO_ROOT}/.buildkite/pipeline.yml"
RUNNER="${REPO_ROOT}/.buildkite/run-bazel.sh"

if [[ ! -f "${PIPELINE}" ]]; then
  echo "error: missing .buildkite/pipeline.yml" >&2
  exit 1
fi

if [[ ! -x "${RUNNER}" ]]; then
  echo "error: .buildkite/run-bazel.sh must be executable" >&2
  exit 1
fi

for executable in \
  "${REPO_ROOT}/.buildkite/run-cli-linux.sh" \
  "${REPO_ROOT}/.buildkite/run-desktop-package-linux.sh" \
  "${REPO_ROOT}/.buildkite/run-desktop-package-macos.sh"
do
  if [[ ! -x "${executable}" ]]; then
    echo "error: ${executable#${REPO_ROOT}/} must be executable" >&2
    exit 1
  fi
done

if command -v buildkite-agent >/dev/null 2>&1; then
  BUILDKITE_AGENT_ACCESS_TOKEN="${BUILDKITE_AGENT_ACCESS_TOKEN:-local-buildkite-dry-run-token}" \
    buildkite-agent pipeline upload \
      --log-level error \
      --dry-run \
      --reject-secrets \
      --format yaml \
      "${PIPELINE}" >/dev/null
elif [[ -n "${BUILDKITE:-}" ]]; then
  echo "error: buildkite-agent is required for Buildkite pipeline dry-run validation" >&2
  exit 1
else
  echo "warning: buildkite-agent not found; skipping Buildkite parser dry-run" >&2
fi

if [[ -d "${REPO_ROOT}/.github/workflows" ]] \
  && find "${REPO_ROOT}/.github/workflows" -type f | grep -q .; then
  echo "error: GitHub Actions workflows are not allowed; use Buildkite" >&2
  exit 1
fi

required_snippets=(
  ".buildkite/validate-pipeline.sh"
  ".buildkite/run-bazel.sh test //:buildkite_config_test //:schemas"
  ".buildkite/run-bazel.sh build --nobuild //:presubmit //:all-rust //:all-web //:e2e-premerge //:release //:release-artifacts"
  ".buildkite/run-bazel.sh test //:all-rust"
  ".buildkite/run-bazel.sh test //:all-web"
  ".buildkite/run-cli-linux.sh"
  "powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File .buildkite/run-cli-windows.ps1"
  ".buildkite/run-desktop-package-linux.sh"
  ".buildkite/run-desktop-package-macos.sh"
  ".buildkite/run-bazel.sh test //:e2e-premerge"
  ".buildkite/run-bazel.sh test //:release"
  ".buildkite/run-bazel.sh build //:release-artifacts"
  "key: \"source-contracts\""
  "key: \"build-graph-analysis\""
  "key: \"rust-all\""
  "key: \"web-all\""
  "key: \"cli-linux-x64\""
  "key: \"cli-windows-x64\""
  "key: \"desktop-linux-x64\""
  "key: \"desktop-macos-arm64\""
  "key: \"desktop-macos-x64\""
  "key: \"browser-premerge-e2e\""
  "key: \"release-proof\""
  "key: \"release-artifacts\""
  "queue: \"windows-x64\""
  "queue: \"release-linux-managed\""
  "queue: \"ctx-mac-gui-shared-arm64\""
  "queue: \"ctx-mac-gui-shared-x64\""
)

for snippet in "${required_snippets[@]}"; do
  if ! grep -Fq "${snippet}" "${PIPELINE}"; then
    echo "error: Buildkite pipeline missing required command: ${snippet}" >&2
    exit 1
  fi
done

if grep -Eiq 'github actions|\.github/workflows|gha' "${PIPELINE}"; then
  echo "error: Buildkite pipeline must not reference GitHub Actions" >&2
  exit 1
fi
