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
