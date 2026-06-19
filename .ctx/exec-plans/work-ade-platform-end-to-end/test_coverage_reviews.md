# Test Coverage Reviews

Record adversarial test coverage reviews and gaps.

## Pending

- Final adversarial coverage review before done-ness review.

## Plugin SDK Slice Review

- Added runtime tests for valid examples, ACP provider JSON fixture validation,
  command source qualification, duplicate plugin/provider IDs, collector direct
  store-write rejection, and deferred contribution rejection when embedded in
  the v1 manifest.
- Added adversarial malformed-manifest tests so invalid JSON-like objects return
  diagnostics instead of throwing.
- Added entrypoint field validation coverage for invalid entrypoint kind,
  non-string args, and non-string environment values.
- Added Bazel `unit_tests` and `typecheck` targets and included SDK unit tests
  in `WEB_TESTS`, closing the initial shifted-left coverage gap.
- Remaining gap: hot reload behavior is not covered by this SDK-only slice and
  still requires the plugin registry/reload implementation slice.

## Work CLI Slice Review

- Added unit coverage for primary `ctx work` parsing and compatibility
  `ctx agent-work` alias parsing.
- Added schema listing coverage and structural validation coverage for
  AgentWork, ChangeSet, Contribution, bundle path safety, and schema version
  rejection.
- Added adversarial redaction coverage for transcript bodies, secret-like
  values, secret-like field names, and absolute paths.
- Added safe inspection coverage for unknown JSON shapes so the CLI does not
  mislabel arbitrary files as `agent-work` and does not print raw secret-like
  fields.
- Added durable diagnostic newline escaping coverage.
- Updated Bazel bin smoke coverage so root help, `ctx work --help`, compatibility
  `ctx agent-work --help`, schema printing, and alias schema printing are
  exercised.
- Remaining gap: `ctx work list/show/capture/export/import` intentionally return
  local diagnostics in this slice. Real store-backed list/show, import/export,
  and capture paths still require the next Work CLI/storage slice.

## Work CLI Adversarial Review Fixes

- Closed high-risk coverage gap where transcript-like event records could leak
  raw assistant/user text through `payload_json`, `content_fragment`, `delta`,
  `full_content`, or nested message-like keys during `ctx work
  redaction-preview`.
- Closed high-risk coverage gap where `ctx work validate --kind
  plugin-manifest` only checked shallow fields by parsing into the Rust
  `PluginManifest` model and exercising structural validation in CLI unit
  tests.
- Added negative coverage for unknown plugin manifest fields, including command
  contribution fields that are outside the public v1 manifest schema.
- Extended Bazel bin smoke coverage to print the `work-bundle` schema and reject
  bundle object paths containing `..`.
- Remaining architectural gap: plugin manifest rules still exist in the Rust
  model, daemon loader, CLI validator, and TypeScript SDK. A later slice should
  either generate validation from one schema source or add a shared contract
  parity test across those implementations.

## Store-Backed Work CLI Slice Review

- Added unit coverage for local store-backed `ctx work list`, `ctx work show`,
  `ctx work export`, and `ctx work import` against a real temp `StoreManager`
  and workspace store.
- Covered full-local export/import round trip so persisted change sets and
  contributions survive across data roots when the target workspace identity is
  explicitly registered.
- Covered workspace-mismatch rejection before writing so imports cannot
  accidentally merge records into a different local workspace.
- Preserved JSON-only stdout for `--json` and stdout export modes so
  machine-readable output is parseable without stripping diagnostics.
- Remaining gap: live `ctx work capture` remains intentionally diagnostic-only
  until the daemon/session capture design lands; it should capture durable Work
  events through daemon-owned semantics rather than direct ad hoc store writes.

## Plugin Contribution Collision Slice Review

- Added daemon inventory coverage for duplicate provider contribution IDs across
  different plugins. The affected plugins become load errors and do not appear
  in the provider extension registry.
- Added daemon inventory coverage for duplicate command and UI surface IDs
  across plugins. These stay loaded and registered because current execution and
  registry records are plugin-qualified, but each plugin receives warning
  diagnostics.
- Re-ran the broader Bazel daemon unit partition, not just the new focused
  tests, to catch runtime and route-adjacent regressions.
- Remaining gaps: bad-manifest reload last-good behavior, in-flight command
  reload/remove behavior, and plugin diagnostics integration into a broader
  diagnostics surface still need later slices.

## Plugin Last-Good Reload Slice Review

- Added daemon inventory coverage for recoverable bad-manifest reload keeping
  the last good registry active and avoiding repeated diagnostic accumulation.
- Added regression coverage for bad-manifest last-good preservation interacting
  with a newly loaded duplicate plugin ID. Both plugins become load errors and
  the duplicate command is excluded from the registry.
- Added regression coverage for bad-manifest last-good preservation interacting
  with a newly loaded duplicate provider ID. Both plugins become load errors and
  the duplicate provider is excluded from the registry.
- Added regression coverage for bad-manifest last-good preservation interacting
  with a newly loaded duplicate runtime ID. Both plugins become load errors and
  the duplicate runtime is excluded from the registry.
- Remaining gaps: in-flight plugin command behavior across reload/remove and
  ADE-visible plugin diagnostics still need later slices.

## Workbench Plugin Contribution Projection Review

- Added frontend projection coverage for current plugin `ui_surfaces` registry
  records into source-labeled Workbench contribution candidates.
- Covered malformed record filtering, unsupported surface compatibility,
  loading/error/empty/ready/fallback states, removed-plugin fallback, and
  delimiter-safe plugin template ID round trips.
- Reviewer found no blockers and confirmed the slice remains frontend-only and
  data-only, without arbitrary plugin UI execution.
- Remaining gap: this is not wired into persisted template selection or rendered
  plugin panels yet; that should wait for SDK/schema parity and a concrete
  Workbench contribution runtime.

## Transactional Work Import Review

- Added store-level coverage for transactional all-or-none import across change
  sets and contributions.
- Added dry-run validation coverage that exercises the same transactional
  relation checks and rolls them back instead of only validating JSON and
  workspace IDs.
- Added cross-workspace ID collision coverage for batch import, including
  rollback of a valid earlier record when a later contribution collides.
- Added CLI coverage for import dry-run catching missing relational references
  without persisting the preceding valid change set.
- Reviewer found no blockers after the slice was narrowed to local import and
  confirmed `ctx work capture` remains intentionally diagnostic-only.
