# Manager Changelog

Record each local commit or integrated worker handoff here.

## Entries

- `d3e24fe` - Plan Work ADE platform completion.
  - Added detailed end-to-end execution plan, completion records, and public
    Work/ADE extension decision docs.
  - Reviewed by Ohm and Locke before commit.
- `5dc809d` - Add composable workbench templates.
  - Added Classic/Kanban/Multipane/Review template infrastructure, persisted
    template state, Workbench split panes, Work task board/detail projection,
    and focused frontend tests.
  - Focused Vitest template/projection suite passed after commit.
- `729d953` - Harden Work artifact paths.
  - Added safe relative path validation for Work artifact/file endpoints in
    schema and store tests.
  - Focused `ctx-store agent_work` tests passed after commit.
- `399b29e` - Harden plugin inventory runtime.
  - Marked duplicate plugin IDs as load errors, preserved command stdin
    BrokenPipe handling, and hardened a relative command test.
  - Focused duplicate-plugin daemon test passed after commit.
- `6a36194` - Add safe local validation wrappers.
  - Added `cargo-safe`, `check-local`, and Makefile `check`/safe `test` target.
  - Shell syntax check passed after commit.
- `3d1b60a` - Document Work and plugin contracts.
  - Added manager-owned blocking contracts for Work namespace compatibility,
    Work source-of-truth/storage semantics, ACP provider plugins, and plugin
    contributions/capabilities.
  - This is the base contract commit for future parallel worker branches unless
    superseded by another manager-owned contract commit.
- `8123c74` - Tighten Work plugin contracts.
  - Adds durable diagnostic events, old control-plane historical import
    boundaries, local ACP v1 conformance target, approved importer write actions,
    and ID-class collision rules.
- `ee4b219` - Add Workbench template visual coverage.
  - Adds Playwright visual coverage for Classic, Kanban, Multipane, Review,
    dense task lists, and multipane split/focus/resize states.
  - Fixes the HTML topbar wrapper so the topbar host owns the shell grid area
    and the template switcher does not collapse into the sidebar column.
- `308fa83` - Add repo-local plugin SDK.
  - Adds `@ctx/plugin-sdk` as a repo-local, publishable-later TypeScript package
    for current v1 plugin manifests.
  - Adds ACP provider, review panel/command, importer action, deferred
    contribution examples, JSON-safe validation, adversarial tests, and Bazel
    coverage in the web test taxonomy.
- `a703a9d` - Add local Work CLI checks.
  - Renames the public local CLI surface to `ctx work` while keeping
    `ctx agent-work` as a compatibility alias.
  - Adds local schema listing/printing, structural JSON validation, safe
    metadata inspection, redaction preview, durable local diagnostics, and
    explicit not-implemented diagnostics for list/show/capture/export/import.
- `c2af929` - Harden Work CLI validation and redaction.
  - Addresses adversarial review findings by routing plugin manifest validation
    through the Rust manifest model, rejecting unknown plugin manifest fields,
    extending transcript-like event redaction, and adding bundle schema smoke
    coverage.
- `c9d2505` - Add store-backed Work CLI operations.
  - Wires `ctx work list`, `ctx work show`, `ctx work export`, and `ctx work
    import` to the existing local `StoreManager` Work graph.
  - Keeps `ctx work capture` as an explicit local diagnostic because live
    capture needs daemon/session semantics rather than direct store wrapping.
  - Defaults exports to safe redaction, supports explicit full-local export,
    rejects cross-workspace imports, and keeps hosted/team/enforcement state out
    of the public local CLI.
- `e659918` - Add plugin contribution collision diagnostics.
  - Adds daemon inventory diagnostics for cross-plugin contribution ID
    collisions.
  - Treats duplicate provider/runtime IDs as hard load errors because they are
    authority-bearing.
  - Keeps duplicate command/UI IDs loaded but emits warnings because current
    execution and registry surfaces are plugin-qualified.
- `14d28c6` - Preserve last-good plugin reloads.
  - Keeps the last good loaded plugin active across recoverable manifest
    read/parse/validation failures.
  - Runs duplicate plugin/provider/runtime finalization after preservation so
    last-good recovery cannot bypass authority-bearing collision checks.
  - Adds regression tests for bad-manifest recovery, duplicate plugin ID, and
    duplicate provider/runtime ID interactions.
- `725edbf` - Define ACP harness starter hooks.
  - Integrated worker branch `ctx/harness-hooks-20260619`.
  - Documents the future harness starter kit as ACP-compatible modular
    primitives rather than a new ctx-specific agent protocol.
  - Adds example module boundaries for optional shell/file/edit/sandbox,
    transcript, artifact, and Work capture helpers.
- `87cdd71` - Add host-level Cargo safety lock.
  - Updates `cargo-safe.sh` so local Rust validation serializes through a
    host-level Cargo lock and runs under low I/O priority by default.
  - Documents the no-direct-concurrent-Cargo process rule for local development
    agents.
- `2174364` - Add Workbench plugin UI contribution projection.
  - Integrated worker branch `ctx/workbench-contrib-20260619`.
  - Adds frontend-only projection for current plugin `ui_surfaces` registry
    records into source-labeled Workbench contribution candidates.
  - Keeps the slice data-only: no arbitrary React/webview/plugin execution.
- `de4488f` - Harden Workbench plugin template IDs.
  - Removes future-looking action IDs until SDK/schema parity defines actions.
  - Uses delimiter-safe encoding for persisted plugin template IDs.
- `0fd4576` - Add transactional Work import.
  - Integrates worker branch `ctx/work-import-txn-v2-20260619`.
  - Adds store-level transactional import for local Work change sets and
    contributions, plus dry-run validation that executes the same transaction
    and rolls it back.
  - Keeps `ctx work capture` diagnostic-only and keeps hosted/team/enforcement
    state out of public local import.
  - Closes reviewer gaps for dry-run relational validation and cross-workspace
    ID collision coverage.
- `a4a53be` - Add declarative plugin contribution contract.
  - Integrates worker branch `ctx/plugin-sdk-declarative-v2-20260619`.
  - Adds host-owned declarative Workbench contribution buckets to the public
    manifest, SDK, JSON schema, ctx-types, and Rust plugin model.
  - Keeps arbitrary UI execution and redaction/export processors deferred.
  - Makes daemon/Rust manifest parsing fail closed on unknown fields and
    enforces toolbar command references against declared plugin commands.
- `4c8f7c0` - Project declarative plugin buckets.
  - Integrates worker branch `ctx/plugin-registry-declarative-buckets-20260619`.
  - Adds daemon extension-registry projection for `templates`,
    `toolbar_actions`, `artifact_renderers`, `card_renderers`,
    `detail_sections`, and `review_sections`.
  - Keeps provider/runtime collisions authority-bearing while treating
    declarative bucket collisions as source-labeled advisory diagnostics.
- `e86cf92` - Cover declarative plugin collision warnings.
  - Adds direct daemon coverage for duplicate declarative Workbench IDs that
    remain valid through namespace-prefix plugin IDs.
  - Confirms affected plugins stay loaded, receive warning diagnostics, and
    keep both plugin-qualified registry registrations.
- `cd5f76e` - Project declarative workbench contributions.
  - Integrates worker branch `ctx/workbench-declarative-contrib-20260619`.
  - Adds inert frontend projection for the six declarative Workbench registry
    buckets, with source labels, bucket grouping, compatibility states, and
    loading/error/empty fallbacks.
- `903a55c` - Align declarative workbench registry projection.
  - Removes temporary local registry casts now that daemon/ctx-types expose the
    declarative buckets directly.
  - Aligns compatible template/renderer IDs with the public SDK examples and
    adds store normalization coverage so buckets are preserved through daemon
    loading.
- `65d9e22` - Label slash command sources.
  - Integrates worker branch `ctx/command-source-labels-20260619`.
  - Adds source metadata for provider/plugin slash commands and renders compact
    source labels in composer autocomplete while preserving command insertion.
- `c9d0eb1` - Harden slash command source labels.
  - Aligns provider labels with the harness catalog, covers provider/plugin
    command collision insertion with namespaced plugin tokens, and prevents
    duplicate autocomplete keys if duplicate descriptors leak through.
- `aa0db81` - Add local plugin CLI dev loop.
  - Adds `ctx plugin validate`, `ctx plugin list`, and `ctx plugin reload` as
    local scanner/dev-loop commands on the single public `ctx` binary.
  - Routes validation through the shared Rust plugin manifest model and keeps
    list/reload output bounded to inventory metadata and diagnostics.
  - Labels list/reload output as `local_scan`, aligns default/env root
    semantics with the daemon runtime, and documents that this slice does not
    mutate a running daemon.
- `0908e58` - Route E2E cargo through safe wrapper.
  - Fixes managed Playwright local-build server launches so they use
    `scripts/dev/cargo-safe.sh` by default on Unix when available.
  - Keeps Bazel-runfiles E2E launches Cargo-free and supports explicit
    `CTX_E2E_CARGO_BIN` override or `CTX_E2E_DISABLE_CARGO_SAFE=1` opt-out.
  - Documents the managed-Playwright Cargo path in the local development
    validation notes.
- `3ac804e` - Record E2E cargo safety integration.
  - Records the managed Playwright Cargo safety fix in the execution-plan
    changelog, validation log, and SDLC review ledger.
- `9bac206` - Update plugin contribution contract status.
  - Marks the declarative plugin contribution contract and inert Workbench
    projection status in public docs.
- `6105306` - Wire plugin route integration test.
  - Adds route-level coverage for plugin inventory/registry/reload/command
    surfaces so the daemon plugin API is exercised outside pure model tests.
- `e53643a` - Surface workbench contribution projections.
  - Renders source-labeled plugin contribution candidates in the Workbench
    shell using host-owned projection data only.
- `a927091` - Fix agent work export import safety.
  - Tightens local Work export/import safety around redaction, workspace
    matching, and transactional store import semantics.
- `1d3ce74` - Align work CLI plugin validation.
  - Removes duplicate ad hoc plugin-manifest shape checks from `ctx work
    validate` and delegates to the shared strict Rust plugin manifest model.
  - Adds coverage that declarative Workbench contribution manifests validate
    through the Work CLI path.
- `d630872` - Add Workbench contribution visual coverage.
  - Seeds a local visual-test plugin and adds Playwright coverage for plugin
    contribution panels in desktop-tight and narrow Kanban layouts.
  - Adjusts contribution row layout so source labels and captions remain
    readable in constrained panels.
- `fa30b3f` - Unexpose org policy routes from public local API.
  - Removes organization policy, daemon enrollment, and policy snapshot route
    registrations from the public local HTTP API.
  - Rewrites route tests to assert those endpoints are unavailable in the
    public local app and updates public docs toward local-first scope.
- `a58ed7d` - Strengthen schema and type validation gates.
  - Adds AJV Draft 2020-12 schema compilation with local `$ref` preflight,
    schema unit tests, Bazel schema tests, ctx-types Bazel typecheck, and a
    quick local validation mode.
- `dd5a4ec` - Tighten work CLI schema validation.
  - Makes `ctx work validate` reject unknown public Work fields and invalid
    enum/reference shapes locally instead of only checking shallow JSON shape.
- `d910367` - Remove stale Supabase web dependency.
  - Removes unused `@supabase/supabase-js` package metadata and Bazel data
    labels from the public web app after verifying no JavaScript imports remain.
- `e5196c8` - Record latest local validation slices.
  - Records the local validation state, review ledgers, and UI artifact notes
    for the Workbench/plugin contribution and public-boundary slices.
- `94ffce1` - Document host-owned Workbench contribution IDs.
  - Updates the public plugin contribution contract with the supported
    host-owned template and renderer IDs.
- `9ab812f` - Remove hosted control-plane code from public core.
  - Removes public org-policy, daemon enrollment, hosted policy snapshot, and
    run-archive ingest crates/routes/contracts from the public core.
  - Keeps old local SQLite stores openable through reserved migration slots and
    migration repair tests.
  - Validated through affected-crate Rust check/lib-test gates and Buildkite
    source/analysis gates recorded in `validation_log.md`.
- `b0fcd3b` - Remove hosted settings surfaces from public app.
  - Removes public billing, team/enterprise, entitlement, account, and mobile
    access settings sections/controllers/tests from the public app shell.
  - Leaves legal/docs billing mentions and local mobile connection surfaces
    outside the removed hosted settings UI.
- `c546ed7` - Expand Workbench visual plugin coverage.
  - Adds browser visual coverage for source-labeled command rows, unsupported
    contribution diagnostics, plugin hot reload add/change/remove fallback, and
    active draft preservation.
  - Updates the mobile-narrow visual helper to exercise the actual mobile shell
    instead of a squeezed desktop viewport.
- `3d74b92` - Record final local validation for Work ADE branch.
  - Records final local validation evidence, accepted deferrals, review notes,
    and plugin contribution contract status before the dedicated done-ness
    review.
  - Leaves push, PR, merge, hosted/team services, production release, and
    remote Buildkite execution out of local scope.
- `6fc2b2d` - Resolve final review ledger status.
  - Replaces stale pre-final review ledger headings with the completed
    final architecture, security, SDLC, coverage, and done-ness review status.
  - Records reviewer findings that the local validation coverage is credible
    and hosted/team/prod/remote work remains out of scope.
- `9480cc0` - Clear stale final ledger marker.
  - Replaces the stale Cargo safety-lock placeholder with the
    actual `87cdd71` commit.
  - Removes remaining stale pre-final wording from review ledger summaries.
- `f7b773e` - Clarify work capture diagnostic.
  - Updates `ctx work capture` so the diagnostic reflects the current local
    store-backed CLI: capture still needs live daemon/session semantics, while
    `list`, `show`, `export`, and `import` are available as offline local Work
    record commands.
  - Validated with `cargo-safe` `fmt --package ctx-http`, the focused
    `ctx-http --bin ctx agent_work_cli` test filter, and `git diff --check`.
