#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/dashboard-review-dogfood.sh [options]

Build local-only ctx dashboard/review artifacts for work records.

Options:
  --archive PATH         Import a ctx archive fixture.
                         Default: examples/dogfood-dashboard-review-archive.json
  --seed-live            Seed fixture records through ctx CLI commands instead of importing.
  --output DIR           Artifact directory.
                         Default: target/ctx-artifacts/dashboard-review
  --data-root DIR        CTX_DATA_ROOT for the dogfood run.
                         Default: target/tmp/dashboard-review-data
  --skip-screenshots     Do not attempt browser screenshots. Requires
                         --accept-visual-blocker to succeed.
  --accept-visual-blocker TEXT
                         Accept an explicit visual review blocker instead of
                         requiring screenshot artifacts.
  -h, --help             Show this help.
USAGE
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
archive_path="${repo_root}/examples/dogfood-dashboard-review-archive.json"
artifact_dir="${repo_root}/target/ctx-artifacts/dashboard-review"
data_root="${repo_root}/target/tmp/dashboard-review-data"
seed_mode="import"
skip_screenshots=0
accepted_visual_blocker=""

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
    --accept-visual-blocker)
      if [[ $# -lt 2 ]]; then
        printf 'blocker: --accept-visual-blocker requires a reason\n' >&2
        exit 2
      fi
      accepted_visual_blocker="$2"
      shift 2
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

sed_literal() {
  printf '%s' "$1" | sed 's/[#&]/\\&/g'
}

sanitize_share_artifact() {
  local path="$1"
  local tmp
  local escaped_repo
  local escaped_home
  local escaped_data_root

  if [[ ! -f "${path}" ]]; then
    return 0
  fi

  tmp="$(mktemp "${path}.XXXXXX")"
  escaped_repo="$(sed_literal "${repo_root}")"
  escaped_data_root="$(sed_literal "${data_root}")"
  escaped_home="$(sed_literal "${HOME:-}")"
  if [[ -n "${escaped_home}" ]]; then
    sed \
      -e "s#${escaped_data_root}/[^[:space:]\"']*#[REDACTED_PATH]#g" \
      -e "s#${escaped_data_root}#[REDACTED_PATH]#g" \
      -e "s#${escaped_repo}/[^[:space:]\"']*#[REDACTED_PATH]#g" \
      -e "s#${escaped_repo}#[REDACTED_PATH]#g" \
      -e "s#${escaped_home}/[^[:space:]\"']*#[REDACTED_PATH]#g" \
      -e "s#${escaped_home}#[REDACTED_PATH]#g" \
      "${path}" >"${tmp}"
  else
    sed \
      -e "s#${escaped_data_root}/[^[:space:]\"']*#[REDACTED_PATH]#g" \
      -e "s#${escaped_data_root}#[REDACTED_PATH]#g" \
      -e "s#${escaped_repo}/[^[:space:]\"']*#[REDACTED_PATH]#g" \
      -e "s#${escaped_repo}#[REDACTED_PATH]#g" \
      "${path}" >"${tmp}"
  fi
  mv "${tmp}" "${path}"
}

verify_share_artifacts() {
  local leaked=0

  if grep -R -F -q -- "${repo_root}" "${artifact_dir}"; then
    printf 'blocker: dogfood artifact set leaked repo root: %s\n' "${repo_root}" >&2
    leaked=1
  fi
  if [[ -n "${HOME:-}" ]] && grep -R -F -q -- "${HOME}" "${artifact_dir}"; then
    printf 'blocker: dogfood artifact set leaked HOME path: %s\n' "${HOME}" >&2
    leaked=1
  fi
  if grep -R -F -q -- "${data_root}" "${artifact_dir}"; then
    printf 'blocker: dogfood artifact set leaked raw data root: %s\n' "${data_root}" >&2
    leaked=1
  fi
  if grep -R -E -q 'chrome-profile|chrome-cache|firefox-tmp|ctx-dashboard-screenshot' "${artifact_dir}"; then
    printf 'blocker: dogfood artifact set leaked browser scratch paths\n' >&2
    leaked=1
  fi

  if [[ "${leaked}" -ne 0 ]]; then
    return 1
  fi
}

safe_reset_data_root() {
  local default_root="${repo_root}/target/tmp/dashboard-review-data"
  case "${data_root}" in
    "${default_root}"|"${repo_root}/target/tmp/"*)
      rm -rf "${data_root}"
      ;;
  esac
}

seed_live_records() {
  local record_json record_id sparse_json sparse_id failed_json failed_id fixture provider

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

  for provider in codex pi claude; do
    fixture="${repo_root}/tests/fixtures/provider/${provider}.jsonl"
    if [[ -f "${fixture}" ]]; then
      run_ctx capture import-provider --provider "${provider}" --input "${fixture}" --json >"${artifact_dir}/provider-${provider}-import.json"
    fi
  done
}

capture_screenshots() {
  local dashboard_html="$1"
  local screenshot_dir="$2"
  local screenshot_status="$3"
  local module_name=""
  local require_root="${repo_root}/apps/ctx-dashboard/package.json"
  local browser_path="${CTX_DASHBOARD_REVIEW_BROWSER:-}"
  local expected

  mkdir -p "${screenshot_dir}"
  rm -f "${screenshot_dir}"/*.png

  if ! command -v node >/dev/null 2>&1; then
    printf 'blocker: node is not available; dashboard screenshots were not captured\n' | tee "${screenshot_status}"
    return 1
  fi

  if node - "${require_root}" playwright <<'NODE' >/dev/null 2>&1; then
const { createRequire } = require('module');
createRequire(process.argv[2])('playwright');
NODE
    module_name="playwright"
  elif node - "${require_root}" playwright-core <<'NODE' >/dev/null 2>&1; then
const { createRequire } = require('module');
createRequire(process.argv[2])('playwright-core');
NODE
    module_name="playwright-core"
  else
    printf 'blocker: Playwright is unavailable under apps/ctx-dashboard; dashboard screenshots were not captured\n' | tee "${screenshot_status}"
    return 1
  fi

  PLAYWRIGHT_MODULE="${module_name}" \
  PLAYWRIGHT_REQUIRE_ROOT="${require_root}" \
  DASHBOARD_HTML="${dashboard_html}" \
  SCREENSHOT_DIR="${screenshot_dir}" \
  BROWSER_PATH="${browser_path}" \
  node <<'NODE' >"${screenshot_status}" 2>&1
const fs = require('fs');
const http = require('http');
const path = require('path');
const { createRequire } = require('module');
const moduleName = process.env.PLAYWRIGHT_MODULE;
const requireFromDashboard = createRequire(process.env.PLAYWRIGHT_REQUIRE_ROOT);
const { chromium } = requireFromDashboard(moduleName);
const launchOptions = {};
if (process.env.BROWSER_PATH) {
  launchOptions.executablePath = process.env.BROWSER_PATH;
}
const views = [
  { viewport: 'desktop', width: 1440, height: 1100 },
  { viewport: 'mobile', width: 390, height: 1200 },
];
const states = [
  { name: 'overview', tab: null, requiredText: 'Work Records' },
  { name: 'record-detail', tab: 'Records', requiredText: 'Files, artifacts, and summaries' },
  { name: 'timeline', tab: 'Timeline', requiredText: 'Work timeline' },
  { name: 'evidence-failure', tab: 'PR Evidence', requiredText: 'Command evidence' },
  { name: 'search-timeline', tab: 'Search', requiredText: 'Search agent history' },
  { name: 'setup-health', tab: 'Setup Health', requiredText: 'Capture health' },
];
const dashboardRoot = path.resolve(path.dirname(process.env.DASHBOARD_HTML));
const mimeTypes = new Map([
  ['.html', 'text/html; charset=utf-8'],
  ['.js', 'text/javascript; charset=utf-8'],
  ['.css', 'text/css; charset=utf-8'],
  ['.json', 'application/json; charset=utf-8'],
  ['.png', 'image/png'],
]);

function startStaticServer() {
  const server = http.createServer((request, response) => {
    const requestUrl = new URL(request.url, 'http://127.0.0.1');
    const relativePath = decodeURIComponent(requestUrl.pathname === '/' ? '/index.html' : requestUrl.pathname);
    const candidate = path.resolve(dashboardRoot, `.${relativePath}`);
    if (!candidate.startsWith(dashboardRoot + path.sep) && candidate !== path.join(dashboardRoot, 'index.html')) {
      response.writeHead(403);
      response.end('forbidden');
      return;
    }
    fs.readFile(candidate, (error, content) => {
      if (error) {
        response.writeHead(404);
        response.end('not found');
        return;
      }
      response.writeHead(200, { 'content-type': mimeTypes.get(path.extname(candidate)) || 'application/octet-stream' });
      response.end(content);
    });
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      resolve({ server, url: `http://127.0.0.1:${address.port}/index.html` });
    });
  });
}
(async () => {
  const { server, url } = await startStaticServer();
  const browser = await chromium.launch(launchOptions);
  try {
    for (const view of views) {
      const page = await browser.newPage({ viewport: { width: view.width, height: view.height } });
      await page.goto(url, { waitUntil: 'networkidle' });
      for (const state of states) {
        if (state.tab) {
          await page.getByRole('tab', { name: state.tab }).click();
        }
        await page.getByText(state.requiredText).first().waitFor({ state: 'visible', timeout: 5000 });
        if (state.name === 'setup-health') {
          await page.getByText(/codex|claude|pi|opencode/i).first().waitFor({ state: 'visible', timeout: 5000 });
        }
        if (state.name === 'evidence-failure') {
          await page.locator('.command-card-danger, .evidence-danger, .badge-danger').first().waitFor({ state: 'visible', timeout: 5000 });
        }
        await page.screenshot({
          path: path.join(process.env.SCREENSHOT_DIR, `${view.viewport}-${state.name}.png`),
          fullPage: true,
        });
      }
      await page.close();
    }
  } finally {
    await browser.close();
    server.close();
  }
  console.log(`captured ${views.length * states.length} dashboard screenshots`);
})().catch((error) => {
  console.log(`blocker: Playwright/Chromium launch failed; dashboard screenshots were not captured: ${error.message}`);
  process.exit(1);
});
NODE
  sanitize_share_artifact "${screenshot_status}"
  cat "${screenshot_status}"
  for expected in \
    desktop-overview.png \
    desktop-record-detail.png \
    desktop-timeline.png \
    desktop-evidence-failure.png \
    desktop-search-timeline.png \
    desktop-setup-health.png \
    mobile-overview.png \
    mobile-record-detail.png \
    mobile-timeline.png \
    mobile-evidence-failure.png \
    mobile-search-timeline.png \
    mobile-setup-health.png; do
    if [[ ! -s "${screenshot_dir}/${expected}" ]]; then
      printf 'blocker: expected dashboard screenshot missing or empty: %s\n' "${expected}" | tee -a "${screenshot_status}"
      return 1
    fi
  done
}

write_visual_evidence_manifest() {
  local path="$1"
  local visual_status="$2"
  local blocker="$3"

  {
    printf '{\n'
    printf '  "schema_version": 1,\n'
    printf '  "kind": "dashboard_visual_evidence",\n'
    printf '  "visual_status": "%s",\n' "$(json_escape "${visual_status}")"
    printf '  "accepted_visual_blocker": "%s",\n' "$(json_escape "${blocker}")"
    printf '  "screenshot_count": %s,\n' "$(find "${artifact_dir}/screenshots" -maxdepth 1 -type f -name '*.png' 2>/dev/null | wc -l | tr -d '[:space:]')"
    printf '  "desktop_overview": "screenshots/desktop-overview.png",\n'
    printf '  "desktop_record_detail": "screenshots/desktop-record-detail.png",\n'
    printf '  "desktop_timeline": "screenshots/desktop-timeline.png",\n'
    printf '  "desktop_evidence_failure": "screenshots/desktop-evidence-failure.png",\n'
    printf '  "desktop_search_timeline": "screenshots/desktop-search-timeline.png",\n'
    printf '  "desktop_setup_health": "screenshots/desktop-setup-health.png",\n'
    printf '  "mobile_overview": "screenshots/mobile-overview.png",\n'
    printf '  "mobile_record_detail": "screenshots/mobile-record-detail.png",\n'
    printf '  "mobile_timeline": "screenshots/mobile-timeline.png",\n'
    printf '  "mobile_evidence_failure": "screenshots/mobile-evidence-failure.png",\n'
    printf '  "mobile_search_timeline": "screenshots/mobile-search-timeline.png",\n'
    printf '  "mobile_setup_health": "screenshots/mobile-setup-health.png",\n'
    printf '  "screenshot_status": "screenshot-status.txt"\n'
    printf '}\n'
  } >"${path}"
}

main() {
  cd "${repo_root}"
  export CTX_DATA_ROOT="${data_root}"

  mkdir -p "${artifact_dir}" "$(dirname "${data_root}")"
  safe_reset_data_root
  mkdir -p "${data_root}"

  run_ctx setup --no-import --no-open >/dev/null

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
  run_ctx validate >"${artifact_dir}/validate.txt"

  local screenshot_status="${artifact_dir}/screenshot-status.txt"
  local visual_status="captured"
  if [[ "${skip_screenshots}" -eq 1 ]]; then
    printf 'blocker: screenshot capture disabled by --skip-screenshots\n' | tee "${screenshot_status}"
    visual_status="accepted_blocker"
  elif ! capture_screenshots "${artifact_dir}/dashboard/index.html" "${artifact_dir}/screenshots" "${screenshot_status}"; then
    visual_status="accepted_blocker"
  fi
  if [[ "${visual_status}" == "accepted_blocker" ]]; then
    if [[ -z "${accepted_visual_blocker}" ]]; then
      printf 'blocker: dashboard visual screenshots are required; rerun with Playwright available or pass --accept-visual-blocker with a concrete reason\n' >&2
      exit 1
    fi
    printf 'accepted visual blocker: %s\n' "${accepted_visual_blocker}" >>"${screenshot_status}"
  fi
  sanitize_share_artifact "${screenshot_status}"
  write_visual_evidence_manifest "${artifact_dir}/visual-evidence.json" "${visual_status}" "${accepted_visual_blocker}"

  {
    printf '{\n'
    printf '  "schema_version": 1,\n'
    printf '  "local_only": true,\n'
    printf '  "path_policy": "artifact paths are relative to this manifest; raw local data root is intentionally omitted",\n'
    printf '  "data_root": "[LOCAL_DATA_ROOT]",\n'
    printf '  "artifact_dir": ".",\n'
    printf '  "seed_mode": "%s",\n' "$(json_escape "${seed_mode}")"
    printf '  "dashboard": "dashboard/index.html",\n'
    printf '  "report_text": "report.txt",\n'
    printf '  "report_json": "report.json",\n'
    printf '  "context": "context.md",\n'
    printf '  "search": "search.json",\n'
    printf '  "raw_archive_exported": false,\n'
    printf '  "raw_archive_note": "omitted from default dogfood artifacts because ctx export is portable/private and may contain raw command or artifact content",\n'
    printf '  "visual_evidence": "visual-evidence.json",\n'
    printf '  "visual_status": "%s",\n' "$(json_escape "${visual_status}")"
    printf '  "accepted_visual_blocker": "%s",\n' "$(json_escape "${accepted_visual_blocker}")"
    printf '  "screenshot_status": "%s"\n' "$(json_escape "$(tr '\n' ' ' <"${screenshot_status}")")"
    printf '}\n'
  } >"${artifact_dir}/manifest.json"

  verify_share_artifacts

  printf 'dashboard-review artifacts: %s\n' "${artifact_dir}"
  printf 'dashboard: %s\n' "${artifact_dir}/dashboard/index.html"
  printf 'manifest: %s\n' "${artifact_dir}/manifest.json"
}

main "$@"
