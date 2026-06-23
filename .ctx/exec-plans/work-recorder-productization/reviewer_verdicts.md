# Work Recorder Productization Reviewer Verdicts

Updated: 2026-06-22T19:53:06-05:00

## Read-Only Mapper Results

- Product/repo split mapper: complete. Found public branch is already slim and
  ADE-free, but CLI/docs/storage contract are not aligned with the final product
  plan.
- Local schema/capture/search mapper: complete. Found the current model is a
  useful scaffold but not contract-compatible; recommended landing schema/types
  and versioned migrations before capture/search.
- Dashboard/report UI mapper: complete. Found no dashboard or visual test
  surface exists yet; recommended backend view models, local server, web UI, and
  deterministic seeded screenshots.
- Hosted/private mapper: complete. Found reusable private foundations but no Work
  Recorder hosted tables/API; recommended a separate worker and private
  worktree.
- CI/release mapper: complete. Found no Buildkite/release/install/platform
  matrix in this public branch; recommended resource-safe wrappers and a Linux
  Buildkite lane first.

No milestone reviewer verdicts have passed yet.

## Implementation Worker Results

- Core schema/types worker: complete. Added core DTO/enums and passed focused
  library/workspace-lib checks; full integrated checks passed after merge with
  `TMPDIR=/var/tmp/ctxwr`.
- Docs truth-pass worker: complete. Produced README/docs changes in a child
  worktree; integrated into the manager branch.
- CI/release worker: complete. Produced resource-safe scripts and initial
  Buildkite config in a child worktree; integrated into the manager branch.
- Root command CLI worker: complete. Added root commands and hidden
  compatibility aliases; integrated into the manager branch and validated with
  focused CLI tests plus full check.
- Store foundation worker: complete. Added migration/schema/WAL/busy/FTS
  foundation; integrated into the manager branch and validated with full check.
- Capture spool worker: complete. Added `work-record-capture`, capture fixture
  writer/importer, status/validate spool counts, and tests; integrated into the
  manager branch and validated with focused capture tests plus full check.
- VCS/PR worker: complete. Added `work-record-vcs`, Git/jj inspection, remote
  redaction, repo fingerprints, PR URL parsing, root CLI commands, and tests;
  integrated into the manager branch and validated with focused VCS/CLI tests
  plus full check.
- Search/context worker: complete. Added `work-record-search`, redacted search
  packets, `AgentContextPacket` builder, token-budget truncation, share-safe
  dashboard links, CLI wiring, and tests; integrated into the manager branch and
  validated with focused search/CLI tests plus full check.
- Dashboard export worker: complete. Added `ctx dashboard export` and static
  local HTML report generation; integrated into the manager branch and validated
  with focused report tests, dogfood export, desktop/mobile screenshots, and a
  full capped local check.
- Local shims worker: complete. Added reversible Git/jj/gh wrapper shims,
  hidden shim capture writer, capped stdout/stderr capture, install/uninstall
  safety checks, README/docs updates, and tests; integrated into the manager
  branch and validated with focused shim/capture tests plus a full capped local
  check.
- Docs/examples worker: complete. Added Work Recorder README/doc updates,
  `scripts/check-docs.sh`, and two local dogfood examples; integrated into the
  manager branch after reconciling dashboard/shim wording with the current
  shipped surface, then validated with docs checks and example runs.
- Capture auto-import/doctor/repair slice: complete. Added on-demand spool import
  for normal Work Recorder commands, `ctx doctor`, `ctx repair`, docs updates,
  and focused tests; validated with focused CLI/capture tests and full capped
  local check. Awaiting adversarial local product/security review.

## Milestone Review Results

- Architecture/data model reviewer on head `eb0d8f9`: FAIL.
  - Blocking issues:
    - generated Work Record/evidence IDs were UUIDv4 instead of UUIDv7;
    - public JSON outputs were not consistently schema-versioned, and
      `ctx context --json` did not emit the public `AgentContextPacket`;
    - core data-root helpers did not expose `blobs/`, `inbox/`, and
      `device.json`;
    - evidence output remained inline in SQLite and evidence could be unattached.
  - Resolution status:
    - targeted fixes are implemented locally;
    - focused/full/release dry-run checks passed;
    - fixes committed at `b7abdca`.
- Architecture/data model reviewer on head `b7abdca`: FAIL.
  - Blocking issues:
    - archive JSON lacked a top-level `schema_version`;
    - generated context output upgraded default local-only records to
      `reportable`;
    - `ctx evidence run --json` printed the raw in-memory evidence object before
      store sanitization;
    - evidence stored stdout/stderr as separate artifacts but attached only one
      `artifact_id`, with inconsistent `raw` redaction state for safe previews;
    - legacy migration and archive import could bypass artifact-backed output.
  - Resolution status:
    - targeted fixes are implemented locally;
    - focused/full/release dry-run checks passed;
    - fixes committed at `77d227f`.
- Architecture/data model reviewer on head `77d227f`: FAIL.
  - Blocking issue:
    - JSON archives exported evidence safe previews but did not include artifact
      rows, blob payloads, or evidence/artifact link data, so export/import
      could not preserve full artifact-backed stdout/stderr content.
  - Resolution status:
    - targeted archive payload fixes are implemented locally;
    - focused/full/release dry-run checks passed;
    - fixes committed at `6c33fb1`.
- Architecture/data model reviewer on head `6c33fb1`: PASS.
  - No blockers found.
  - Confirmed archive export/import now carries full stdout/stderr payloads via
    artifact records while persisted evidence rows keep safe previews.
  - Follow-up concerns:
    - archive artifact `content` is string-only and not sufficient for future
      binary artifact kinds;
    - add an explicit archive round-trip test with both stdout and stderr
      payloads.
  - Resolution status:
    - both-stream archive round-trip test added locally and validated;
    - binary archive payload support remains future work for non-text artifacts.
- Manager visual review on uncommitted dashboard hardening over `a5b63b9`: PASS.
  - Reviewed:
    - `/var/tmp/ctxwr-dashboard/dashboard.png`
    - `/var/tmp/ctxwr-dashboard/dashboard-mobile.png`
  - Initial screenshot exposed raw absolute workspace path and token-like command
    text. The dashboard was hardened to render safe workspace labels and redacted
    command fragments before this PASS.
  - Remaining requirement:
    - a separate adversarial UI/product/security review must run after the
      docs/shims/hosted tracks are integrated.

Required reviewer categories from the plan:

- architecture/data model;
- capture fidelity/failure mode;
- security/privacy;
- hosted/API/access control;
- UI visual;
- agent-access/search;
- docs/claims;
- CI/release;
- SDLC/process;
- final done-ness.

## 2026-06-22 Adversarial Review Results On `de1c718`

- Hosted/private staging gap reviewer: BLOCKED.
  - Public local Work Recorder implementation is coherent and docs honestly
    mark hosted/team sync as not implemented.
  - Private `ctx-private` has reusable Neon/Cloudflare foundations but no
    tracked Work Recorder hosted implementation, staging URL, migration proof,
    R2 proof, or hosted security review.
  - Resolution status:
    - private hosted implementation has been split into a bounded worker track;
    - public local work continues independently.
- CI/release/platform reviewer: BLOCKED.
  - Repo-owned pipeline shape was coherent, but live Buildkite evidence was
    missing.
  - Bazel could silently skip in CI if missing.
  - macOS/Windows lanes proved release dry-run builds only, not native smoke.
  - Resolution status:
    - public patch now makes the Buildkite Bazel lane require Bazel/Bazelisk;
    - public patch adds macOS arm64, macOS x64, and Windows x64 native smoke
      lanes before release dry-runs;
    - live Buildkite URLs remain pending.
- Local product/security reviewer: FAIL.
  - Dashboard record fields and tag summary could render raw record content,
    including secrets and local paths.
  - Default Markdown context could render raw record/query/tag/body/command
    content.
  - JSON search/context dropped evidence stdout/stderr-only matches after FTS.
  - Resolution status:
    - public patch adds shared share-safe text redaction;
    - dashboard and Markdown context render share-safe fields;
    - JSON search/context now include evidence-output-only matches;
    - focused and full capped local validation passed.
