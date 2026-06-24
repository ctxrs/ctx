# Codex Rich Session Import Follow-Up

## Scope

Focused follow-up on `ctxrs/ctx` branch `work-record` to make Codex session
JSONL imports richer and more useful for Work Records. This pass does not
reopen ADE, hosted/team, or release scope.

## Starting Point

- Starting head: `571de0b26fb8a073ae1ace74dfe86a593e34b7b4`
- Prior Buildkite evidence: public release verification build 90 was green for
  that starting head.

## Target Outcomes

- Codex `~/.codex/sessions` JSONL import normalizes reliable Codex rollout
  events beyond user/assistant messages.
- Tool calls, command outputs, reasoning summaries, lifecycle notices, and
  parent/child session relationships are persisted as first-class Work Record
  events where safe.
- `ctx search`, `ctx context`, `ctx report --format json`, and dashboard export
  expose useful share-safe previews for imported Codex activity.
- Redacted fixtures and tests cover representative rich Codex event shapes.
- Real local Codex corpus dogfood runs against a temporary `CTX_DATA_ROOT`
  without committing private transcript content.
- Provider docs accurately describe normalized and raw-only Codex fidelity.

## Workstreams

- Capture/fixtures worker: rich Codex JSONL normalization and capture tests.
- Report/search/dashboard worker: nested provider event previews and report JSON
  event exposure.
- Manager integration: docs, dogfood, visual review, serialized validation,
  final review, branch push.

## Implementation Status

- 2026-06-24T03:30:44Z: Follow-up started from clean `work-record` head
  `571de0b26fb8a073ae1ace74dfe86a593e34b7b4`.
- Exploratory review found existing schema can carry the richer data without a
  migration.
- Exploratory review found the current importer explicitly drops Codex
  `function_call`, `custom_tool_call`, `web_search_call`,
  `function_call_output`, `custom_tool_call_output`, and `reasoning` rows.
- Implemented rich Codex session JSONL import for safe, reliable Codex rollout
  items:
  - `response_item.message` for user/assistant messages;
  - `response_item.function_call`, `custom_tool_call`, `web_search_call`, and
    `tool_search_call` as first-class tool-call events with bounded argument
    previews;
  - `function_call_output`, `custom_tool_call_output`, and `tool_search_output`
    as tool-output events;
  - `exec_command` call outputs as command-output events plus normalized
    command `runs` with exit status and duration when Codex records those
    fields;
  - reasoning summaries as summary events while withholding encrypted reasoning
    payloads;
  - safe lifecycle notices such as task start/complete, compaction,
    token-count, patch-apply, and web-search completion notices;
  - existing parent/child session edges remain preserved where Codex records
    parent session identifiers.
- Added redacted fixture coverage at
  `tests/fixtures/provider-history/codex-rich-sessions/2026/06/24/rich.jsonl`.
- Report/search/dashboard surfaces now expose nested provider event previews,
  command-output previews, safe event reports, and imported command runs. The
  dashboard command table now includes imported command runs, not only explicit
  `ctx evidence run` rows.
- Store search projection behavior was hardened for rich histories:
  - `ctx search`/`ctx context` no longer hydrate every record after FTS already
    returned query candidates;
  - `Store::open` no longer rebuilds large search projections on every read
    command;
  - partial search projections left by interrupted read commands are not
    opportunistically repaired on open.
- First adversarial review found three narrow blockers, all fixed in
  `c8d86c7`:
  - repeated Codex session tree imports counted an existing parent/child edge
    as newly imported on the second run;
  - the rich Codex fixture contained private-looking absolute paths, a
    token-shaped argument, and an encrypted reasoning payload field;
  - dashboard screenshots showed useful data but had avoidable visual
    weaknesses: mobile tab clipping, command output previews truncated inside a
    table, search results without enough session/run/timestamp context, and no
    explicit chronological session timeline.
- The fixes added a persisted session-edge existence check, sanitized the rich
  fixture, added edge-idempotency assertions, replaced command evidence tables
  with readable cards, wrapped mobile tabs, added chronological provider
  timelines, and enriched dashboard search result context.

## Dogfood

- Real local Codex corpus inspected without committing private content:
  - path: `~/.codex/sessions`
  - files: 8,652 JSONL files
  - size: about 11 GiB
- Default-product bounded import dogfood succeeded on a temporary data root:
  - command: `CTX_DATA_ROOT=target/tmp/codex-rich-bounded-dogfood-root target/debug/ctx capture import-local-providers --json`
  - timing: 375.52 seconds
  - max RSS: 35,804 KiB
  - imported Codex sessions: 85
  - imported Codex events: 21,438
  - failures: 0
  - event mix: 6,824 notices, 6,099 tool calls, 4,649 command outputs, 2,537
    messages, 1,325 tool outputs, 4 summaries
  - normalized command runs: 4,649
- Agent-access proof on the bounded real import succeeded after the search
  projection fixes:
  - `ctx search exec_command --limit 3 --json`: 0.90 seconds, 31,100 KiB max RSS
  - `ctx context "command output" --limit 3 --max-tokens 2000 --json`: 0.60
    seconds, 32,152 KiB max RSS
  - private JSON outputs are stored only under
    `target/ctx-artifacts/codex-rich-session-import/*.private.json` and are not
    committed.
- Explicit unbounded deep import of the full 11 GiB local corpus was attempted
  earlier in this pass and stopped after more than 50 minutes of CPU-active
  import work. This is recorded as a remaining performance limit for explicit
  deep historical backfill. The default setup/import path remains bounded and
  certified above.

## Visual Evidence

Synthetic rich Codex fixture dashboard export:

- `target/ctx-artifacts/codex-rich-session-import/rich-fixture-dashboard/index.html`

Screenshots reviewed manually:

- `target/ctx-artifacts/codex-rich-session-import/screenshots/desktop.png`
- `target/ctx-artifacts/codex-rich-session-import/screenshots/mobile.png`
- `target/ctx-artifacts/codex-rich-session-import/screenshots/providers-desktop.png`
- `target/ctx-artifacts/codex-rich-session-import/screenshots/pr-evidence-desktop.png`
- `target/ctx-artifacts/codex-rich-session-import/screenshots/search-desktop.png`
- `target/ctx-artifacts/codex-rich-session-import/screenshots/workspace-desktop.png`

Manual visual notes:

- Overview is hydrated and shows imported Codex records with rich activity
  preview text.
- Provider view shows provider session metadata, chronological timeline,
  messages, tool calls, command-output events, run metadata, and command
  evidence cards.
- PR/Evidence view shows the imported command preview and output preview in
  readable command cards even when there are no explicit PR links or manual
  evidence rows.
- Mobile layout remains readable; tabs wrap instead of clipping, and command
  previews remain visible.
- Search results now include command exit/duration previews and event
  session/run/timestamp context.

## Validation

Commands run with local resource-safe settings:

- `npm --prefix apps/ctx-dashboard run build`
- `npm --prefix apps/ctx-dashboard test`
- `cargo-lowio build -p ctx --locked`
- `cargo-lowio test -p work-record-capture -p work-record-store -p work-record-search -p work-record-report --locked -- --test-threads 1`
- `cargo-lowio test -p work-record-report --locked -- --test-threads 1`
- `cargo-lowio test -p ctx --test cli --locked -- --test-threads 1`
- `CTX_ARTIFACT_DIR=target/ctx-artifacts/codex-rich-session-import/docs-check ./scripts/check.sh docs`
- `CTX_ARTIFACT_DIR=target/ctx-artifacts/codex-rich-session-import/fmt ./scripts/check.sh fmt`
- `CTX_ARTIFACT_DIR=target/ctx-artifacts/codex-rich-session-import/check ./scripts/check.sh check`
- `CTX_ARTIFACT_DIR=target/ctx-artifacts/codex-rich-session-import/clippy ./scripts/check.sh clippy`
- `git diff --check`

All listed validation commands passed after the implementation and
review-blocker fixes.

## Review Status

- Capture/schema review: PASS at `862f99c`.
  - Reviewer verified repeat Codex session tree import now reports
    `imported_edges: 0`, `skipped_edges: 1` on the second run.
  - Reviewer verified rich Codex import persists first-class session, event,
    run, and command-output rows, with search/report exposure.
- Privacy/redaction review: PASS at `862f99c`.
  - Reviewer verified the rich fixture is sanitized, encrypted reasoning payload
    content is not committed, and CLI/search/context/report/dashboard generated
    outputs do not expose the prior raw markers.
- Dashboard visual review: PASS at `862f99c`.
  - Reviewer verified regenerated screenshots resolve prior visual blockers:
    mobile tabs wrap, command output previews are readable, provider detail has
    a chronological timeline, and search results include session/run/timestamp
    context.
- Final done check: PASS at `05b83fd`.
  - Final reviewer verified branch `work-record` was clean; rich Codex fixture
    content is sanitized; private dogfood outputs remain under untracked
    `target/`; importer, search/context/report/dashboard, docs, dogfood counts,
    validation, and focused reviewer PASSes satisfy this follow-up.
  - Final reviewer reran focused capture, CLI, report, and `git diff --check`
    validations before returning PASS.

## Dashboard Reconciliation Addendum

- Date: 2026-06-24
- Base rich-import head before reconciliation:
  `2fc5d6251cc31140c3c733524e4c3bda2424cafd`
- Dashboard UX source reconciled: `origin/ctx/wr-dashboard-value-ux` at
  `73d6cc4`
- Integration branch: `work-record`

### What Landed

- Reconciled the records-first dashboard framing from
  `ctx/wr-dashboard-value-ux` with the richer Codex/session/search/report work
  from the rich-import pass.
- Preserved rich Work Record slices in `DashboardReport::from_archive()` and
  `dashboard_export_data()`: sessions, runs, events, command-output-derived
  runs, VCS/PR rows, artifacts, files touched, summaries, and evidence metadata.
- Rebuilt the React/Vite static dashboard assets and updated
  `dashboard_static_assets()` to embed the final asset set, including
  `assets/dashboard-Ca36SNSf.js`, the ctx logo, shared JS, and CSS.
- Updated the public dashboard view names to `Overview`, `Records`, `Timeline`,
  `PR Evidence`, `Search`, and `Setup Health`.
- Removed old dashboard framing from the rendered dashboard: no visible
  `React/Vite`, no `Local ctx dashboard`, no top-level provider metric/tab, no
  top-level raw-transcript metric, no prime-header dark-mode toggle, and no old
  `Workspace` / `Providers` / `Status` tabs.
- Kept richer #91 content where it was better than the UX branch:
  chronological timeline, command cards with output previews, rich search event
  context, transcript/tool event previews, child/session labels, PR evidence
  readiness, artifacts, and setup/capture details.
- Replaced long provider cursor/sequence badges in event rows with compact event
  ids when the source sequence is too large to be human useful.
- Tightened the dashboard visual review scripts so the synthetic review lane
  uses `setup --no-import --no-open`, validates the new six-view screenshot set,
  and sanitizes fake-browser failure output before writing it to logs.

### Screenshot Evidence

Final regenerated screenshot bundle:

- `target/ctx-artifacts/dashboard-review-check/screenshots/desktop-overview.png`
- `target/ctx-artifacts/dashboard-review-check/screenshots/desktop-record-detail.png`
- `target/ctx-artifacts/dashboard-review-check/screenshots/desktop-timeline.png`
- `target/ctx-artifacts/dashboard-review-check/screenshots/desktop-evidence-failure.png`
- `target/ctx-artifacts/dashboard-review-check/screenshots/desktop-search-timeline.png`
- `target/ctx-artifacts/dashboard-review-check/screenshots/desktop-setup-health.png`
- `target/ctx-artifacts/dashboard-review-check/screenshots/mobile-overview.png`
- `target/ctx-artifacts/dashboard-review-check/screenshots/mobile-record-detail.png`
- `target/ctx-artifacts/dashboard-review-check/screenshots/mobile-timeline.png`
- `target/ctx-artifacts/dashboard-review-check/screenshots/mobile-evidence-failure.png`
- `target/ctx-artifacts/dashboard-review-check/screenshots/mobile-search-timeline.png`
- `target/ctx-artifacts/dashboard-review-check/screenshots/mobile-setup-health.png`

Manual screenshot review notes:

- Desktop and mobile overview show the ctx logo, `Local agent history`,
  `Work Records`, and user-value signals: needs attention, work records,
  PR-linked, and searchable history.
- Mobile tabs wrap cleanly across two rows and record cards do not clip.
- PR Evidence shows failed and passing command cards, output previews, PR links,
  and safe artifact previews.
- Record detail shows evidence, PR links, agent sessions, commands, timeline,
  transcript preview, tools/raw-payload status, and files/artifacts/summaries.
- Setup Health contains provider/source details under capture health instead of
  promoting provider count as a top-level metric.
- The handoff card uses `Private payloads`, not a top-level raw-transcript
  metric.

### Validation

Final local validation after dashboard reconciliation:

- `npm --prefix apps/ctx-dashboard run build` - passed.
- `npm --prefix apps/ctx-dashboard test` - passed, 16 Playwright tests.
- `cargo-lowio test -p work-record-report dashboard --locked -- --test-threads
  1` - passed, 3 dashboard tests.
- `CTX_ARTIFACT_DIR=target/ctx-artifacts/dashboard-review-check
  ./scripts/check.sh dashboard-report-artifact-review` - passed and captured 12
  screenshots.
- `cargo-lowio test -p work-record-capture -p work-record-store -p
  work-record-search -p work-record-report --locked -- --test-threads 1` -
  passed: 22 capture, 6 report, 5 search, 48 store tests, plus doctests.
- `cargo-lowio test -p ctx --test cli --locked -- --test-threads 1` - passed,
  64 CLI tests.
- `CARGO=cargo-lowio
  CTX_ARTIFACT_DIR=target/ctx-artifacts/dashboard-reconcile-hygiene-final
  ./scripts/check.sh fmt docs check clippy` - passed.
- Stale dashboard text scan passed except for the intentional negative
  `React/Vite` Playwright assertion.

### Review Status

- Dashboard/product review: PASS from adversarial reviewer Hume after inspecting
  source and screenshots. Reviewer verified the old bad items are absent, rich
  content remains, share-safe/redacted behavior remains, and mobile tabs wrap
  cleanly.
- Narrow post-polish re-check: PASS from Hume after the final
  `Private payloads` wording and final `dashboard-Ca36SNSf.js` asset hash
  regeneration. Reviewer verified the old dashboard UI remains gone and rich
  command, PR evidence, artifacts, Setup Health, transcript/tool events,
  child-session labels, and share-safe/redacted status remain visible.
