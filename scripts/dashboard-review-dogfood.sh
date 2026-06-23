#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/dashboard-review-dogfood.sh [options]

Build local-only Work Recorder dashboard/review artifacts.

Options:
  --archive PATH         Import a ctx archive fixture.
                         Default: examples/dogfood-dashboard-review-archive.json
  --seed-live            Seed fixture records through ctx CLI commands instead of importing.
  --output DIR           Artifact directory.
                         Default: target/ctx-artifacts/dashboard-review
  --data-root DIR        CTX_DATA_ROOT for the dogfood run.
                         Default: target/tmp/dashboard-review-data
  --skip-screenshots     Do not attempt browser screenshots.
  -h, --help             Show this help.
USAGE
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
archive_path="${repo_root}/examples/dogfood-dashboard-review-archive.json"
artifact_dir="${repo_root}/target/ctx-artifacts/dashboard-review"
data_root="${repo_root}/target/tmp/dashboard-review-data"
seed_mode="import"
skip_screenshots=0

while (($#)); do
  case "$1" in
    --archive)
      if [[ $# -lt 2 ]]; then
        printf 'blocker: --archive requires a path\n' >&2
        exit 2
      fi
      archive_path="$2"
      shift 2
      ;;
    --seed-live)
      seed_mode="live"
      shift
      ;;
    --output)
      if [[ $# -lt 2 ]]; then
        printf 'blocker: --output requires a directory\n' >&2
        exit 2
      fi
      artifact_dir="$2"
      shift 2
      ;;
    --data-root)
      if [[ $# -lt 2 ]]; then
        printf 'blocker: --data-root requires a directory\n' >&2
        exit 2
      fi
      data_root="$2"
      shift 2
      ;;
    --skip-screenshots)
      skip_screenshots=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'blocker: unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

run_ctx() {
  if [[ -n "${CTX_BIN:-}" ]]; then
    if [[ ! -x "${CTX_BIN}" ]]; then
      printf 'blocker: CTX_BIN is set but is not executable: %s\n' "${CTX_BIN}" >&2
      exit 1
    fi
    "${CTX_BIN}" "$@"
  elif [[ -x "${repo_root}/target/debug/ctx" ]]; then
    "${repo_root}/target/debug/ctx" "$@"
  elif command -v cargo >/dev/null 2>&1; then
    cargo run -q -p ctx -- "$@"
  else
    printf 'blocker: no ctx binary found and cargo is not available; set CTX_BIN to a local ctx executable\n' >&2
    exit 1
  fi
}

json_escape() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//$'\n'/\\n}"
  printf '%s' "${value}"
}

safe_reset_default_data_root() {
  local default_root="${repo_root}/target/tmp/dashboard-review-data"
  if [[ "${data_root}" == "${default_root}" ]]; then
    rm -rf "${data_root}"
  fi
}

seed_live_records() {
  local record_json record_id sparse_json sparse_id failed_json failed_id

  record_json="$(run_ctx record \
    --title "Dogfood dashboard review: rich local evidence" \
    --body "Review the dashboard/report path with linked PRs, passing evidence, failing evidence, repeated tags, and redaction-sensitive previews." \
    --tag dogfood \
    --tag dashboard \
    --tag review \
    --tag finished-product \
    --kind task \
    --workspace "${repo_root}" \
    --json)"
  record_id="$(printf '%s\n' "${record_json}" | sed -n 's/.*"id": "\([^"]*\)".*/\1/p' | head -n 1)"
  if [[ -z "${record_id}" ]]; then
    printf 'blocker: failed to read record id from ctx record output\n%s\n' "${record_json}" >&2
    exit 1
  fi
  run_ctx link-pr "${record_id}" https://github.com/ctxrs/ctx/pull/4242 >/dev/null
  run_ctx evidence run --record "${record_id}" -- bash -c 'printf "tests ok\ncoverage: 84%%\nredaction token=ghp_1234567890abcdef should be redacted in previews\n"'

  sparse_json="$(run_ctx record \
    --title "Dogfood dashboard review: sparse metadata sections" \
    --body "Exercise reviewer expectations for sections that remain sparse in CLI dashboard exports." \
    --tag dogfood \
    --tag dashboard \
    --tag sparse-sections \
    --kind task \
    --workspace "${repo_root}" \
    --json)"
  sparse_id="$(printf '%s\n' "${sparse_json}" | sed -n 's/.*"id": "\([^"]*\)".*/\1/p' | head -n 1)"
  if [[ -z "${sparse_id}" ]]; then
    printf 'blocker: failed to read sparse record id from ctx record output\n%s\n' "${sparse_json}" >&2
    exit 1
  fi
  run_ctx evidence run --record "${sparse_id}" -- bash -c 'printf "lint complete\n"; printf "warning: dashboard sparse sections need review\n" >&2'

  failed_json="$(run_ctx record \
    --title "Dogfood dashboard review: failed visual check" \
    --body "Include one failed evidence item so dashboard reviewers can inspect failure styling." \
    --tag dogfood \
    --tag visual \
    --tag failure-path \
    --kind task \
    --workspace "${repo_root}" \
    --json)"
  failed_id="$(printf '%s\n' "${failed_json}" | sed -n 's/.*"id": "\([^"]*\)".*/\1/p' | head -n 1)"
  if [[ -z "${failed_id}" ]]; then
    printf 'blocker: failed to read failed-check record id from ctx record output\n%s\n' "${failed_json}" >&2
    exit 1
  fi
  run_ctx link-pr "${failed_id}" https://github.com/ctxrs/ctx/pull/4243 >/dev/null
  run_ctx evidence run --record "${failed_id}" -- bash -c 'printf "expected failure: screenshot diff threshold exceeded\n" >&2; exit 1' || true
}

capture_screenshots() {
  local dashboard_html="$1"
  local screenshot_dir="$2"
  local screenshot_status="$3"
  local module_name=""
  local browser_path="${CTX_DASHBOARD_REVIEW_BROWSER:-}"

  mkdir -p "${screenshot_dir}"

  if ! command -v node >/dev/null 2>&1; then
    printf 'skip: node is not available; dashboard screenshots were not captured\n' | tee "${screenshot_status}"
    return 0
  fi

  if node -e "require('playwright')" >/dev/null 2>&1; then
    module_name="playwright"
  elif node -e "require('playwright-core')" >/dev/null 2>&1; then
    module_name="playwright-core"
  else
    printf 'skip: Playwright is not available to node; dashboard screenshots were not captured\n' | tee "${screenshot_status}"
    return 0
  fi

  if [[ -z "${browser_path}" ]]; then
    for candidate in chromium chromium-browser google-chrome google-chrome-stable chrome; do
      if command -v "${candidate}" >/dev/null 2>&1; then
        browser_path="$(command -v "${candidate}")"
        break
      fi
    done
  fi

  PLAYWRIGHT_MODULE="${module_name}" \
  DASHBOARD_HTML="${dashboard_html}" \
  SCREENSHOT_DIR="${screenshot_dir}" \
  BROWSER_PATH="${browser_path}" \
  node <<'NODE' >"${screenshot_status}" 2>&1
const fs = require('fs');
const path = require('path');
const { pathToFileURL } = require('url');
const moduleName = process.env.PLAYWRIGHT_MODULE;
const { chromium } = require(moduleName);
const launchOptions = {};
if (process.env.BROWSER_PATH) {
  launchOptions.executablePath = process.env.BROWSER_PATH;
}
(async () => {
  const browser = await chromium.launch(launchOptions);
  const views = [
    { name: 'desktop', width: 1440, height: 1100 },
    { name: 'mobile', width: 390, height: 1200 },
  ];
  for (const view of views) {
    const page = await browser.newPage({ viewport: { width: view.width, height: view.height } });
    await page.goto(pathToFileURL(process.env.DASHBOARD_HTML).href, { waitUntil: 'load' });
    await page.screenshot({
      path: path.join(process.env.SCREENSHOT_DIR, `${view.name}.png`),
      fullPage: true,
    });
    await page.close();
  }
  await browser.close();
  console.log(`captured ${views.length} dashboard screenshots`);
})().catch((error) => {
  console.log(`skip: Playwright/Chromium launch failed; dashboard screenshots were not captured: ${error.message}`);
  process.exit(0);
});
NODE
  cat "${screenshot_status}"
}

main() {
  cd "${repo_root}"
  export CTX_DATA_ROOT="${data_root}"

  mkdir -p "${artifact_dir}" "$(dirname "${data_root}")"
  safe_reset_default_data_root
  mkdir -p "${data_root}"

  run_ctx setup >/dev/null

  if [[ "${seed_mode}" == "import" ]]; then
    if [[ ! -f "${archive_path}" ]]; then
      printf 'blocker: archive fixture not found: %s\n' "${archive_path}" >&2
      exit 1
    fi
    run_ctx import --input "${archive_path}" --overwrite
  else
    seed_live_records
  fi

  run_ctx report >"${artifact_dir}/report.txt"
  run_ctx report --format json >"${artifact_dir}/report.json"
  run_ctx context dogfood >"${artifact_dir}/context.md"
  run_ctx search dogfood --json >"${artifact_dir}/search.json"
  run_ctx dashboard export --output "${artifact_dir}/dashboard"
  run_ctx export --output "${artifact_dir}/work-records.json"
  run_ctx validate >"${artifact_dir}/validate.txt"

  local screenshot_status="${artifact_dir}/screenshot-status.txt"
  if [[ "${skip_screenshots}" -eq 1 ]]; then
    printf 'skip: screenshot capture disabled by --skip-screenshots\n' | tee "${screenshot_status}"
  else
    capture_screenshots "${artifact_dir}/dashboard/index.html" "${artifact_dir}/screenshots" "${screenshot_status}"
  fi

  {
    printf '{\n'
    printf '  "schema_version": 1,\n'
    printf '  "local_only": true,\n'
    printf '  "data_root": "%s",\n' "$(json_escape "${data_root}")"
    printf '  "artifact_dir": "%s",\n' "$(json_escape "${artifact_dir}")"
    printf '  "seed_mode": "%s",\n' "$(json_escape "${seed_mode}")"
    printf '  "dashboard": "%s",\n' "$(json_escape "${artifact_dir}/dashboard/index.html")"
    printf '  "report_text": "%s",\n' "$(json_escape "${artifact_dir}/report.txt")"
    printf '  "report_json": "%s",\n' "$(json_escape "${artifact_dir}/report.json")"
    printf '  "context": "%s",\n' "$(json_escape "${artifact_dir}/context.md")"
    printf '  "search": "%s",\n' "$(json_escape "${artifact_dir}/search.json")"
    printf '  "archive": "%s",\n' "$(json_escape "${artifact_dir}/work-records.json")"
    printf '  "screenshot_status": "%s"\n' "$(json_escape "$(tr '\n' ' ' <"${screenshot_status}")")"
    printf '}\n'
  } >"${artifact_dir}/manifest.json"

  printf 'dashboard-review artifacts: %s\n' "${artifact_dir}"
  printf 'dashboard: %s\n' "${artifact_dir}/dashboard/index.html"
  printf 'manifest: %s\n' "${artifact_dir}/manifest.json"
}

main "$@"
