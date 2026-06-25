# Native Provider Import Coverage ExecPlan

## Metadata

- Workspace: `/home/daddy/code/ctx-multi-repo-workspace`
- Repo: `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/search-sdlc-maturity`
- Branch: `ctx/search-sdlc-maturity`
- Remote target: `origin/ctx/search-sdlc-maturity`
- Started: 2026-06-25
- Status: in progress

## Purpose

Complete the native local-history import coverage effort for the search-only
public ctx CLI while preserving the product boundary from the search SDLC
maturity plan.

The support bar is strict: a provider is supported only when ctx discovers or
imports that provider's real persisted local history format, with sanitized
native fixtures and hermetic tests. Normalized provider JSONL may remain a
developer/test input, but it is not user-facing provider support.

## Guardrails

- Use this manual public ctx worktree as the integration branch.
- Do not use control-plane or Codex automatic parent worktree mode.
- Do not vendor, clone, or copy child repos wholesale.
- Default tests must be hermetic and static: no network, no OpenRouter key, no
  Infisical requirement, and no hidden LLM calls.
- OpenRouter generation, if retained, must be a developer script for drafting
  static fixtures only, not a default Bazel/CI/live gate.
- Discovery/import must fail closed. Missing native history must produce a
  provider-specific message rather than falling back to normalized fixtures.
- SQLite/database providers must be opened read-only without mutating WAL/auth
  stores and must report locked/corrupt cases predictably.
- Provider docs and `docs/provider-support-matrix.json` must claim only proven
  native support.

## Target Providers

- Codex
- Pi
- Claude
- OpenCode
- Antigravity
- Gemini
- Cursor
- Copilot CLI
- Factory AI Droid
- Amp

## Milestones

### 1. Baseline And Branch Archaeology

Status: completed

- Inspect existing provider-related worktrees/branches for reusable native
  importer, registry, fixture, and docs work.
- Identify which providers have real native format evidence versus normalized
  placeholders.
- Record integration candidates and blockers.

### 2. Provider Source Registry And Import Contract

Status: completed

- Introduce or complete a provider source registry with provider id, source
  format, candidate paths, env overrides, probe/discovery function,
  parser/adapter factory, catalog support, and retention/redaction policy.
- Keep normalized envelopes internal while making native parsers first-class.
- Ensure CLI discovery and explicit `--path` behavior use the registry and fail
  closed.

### 3. Native Parser And Fixture Coverage

Status: completed

- Harden Codex and Pi.
- Add proven native parser/discovery support for providers where exact local
  path/schema and sanitized fixtures are available.
- Leave unproven providers blocked/unsupported with explicit reasons.
- Add fixtures under `tests/fixtures/provider-history/<provider>/<format-version>/`
  mirroring real disk layout, with README metadata.

### 4. Catalog, Storage, And Fresh-Home E2E

Status: completed

- Generalize catalog metadata across providers: file size, mtime, content hash,
  DB schema fingerprint where relevant, native session id, parent id, source
  cursor, and import status.
- Ensure parser output includes stable provider session/event ids, native
  cursors, source citations, bounded previews, metadata where available, and
  malformed-input errors.
- Add fresh-home E2E for `setup -> sources -> import -> list -> search -> show
  -> context -> status -> doctor -> validate` using static fixtures.

### 5. Docs, Matrix, And OpenRouter Cleanup

Status: completed

- Update provider docs/matrix so native support claims match evidence.
- Reclassify normalized-only or unproven providers as blocked/unsupported.
- Ensure OpenRouter generation is developer-only and out of default Bazel/CI
  gates.
- Audit default binary/release path for dashboard/shim/PR/publish/evidence
  surfaces.

### 6. Review, Validation, Commit, Push

Status: in progress

- Run architecture/security, provider-fidelity, docs/matrix, and final
  done-criteria reviews.
- Run relevant Bazel checks first-class where available.
- Commit coherent slices and push to `origin/ctx/search-sdlc-maturity`.

## Progress Log

- 2026-06-25: Created plan. Confirmed target branch worktree exists at
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/search-sdlc-maturity`.
  Baseline docs currently mark Codex and Pi as native/importable and the other
  target providers as normalized-only.
- 2026-06-25: Completed branch archaeology. Existing provider branches are
  already represented in the target branch or are stale/high-conflict. No hidden
  native long-tail importer was available to merge.
- 2026-06-25: Added a provider source registry in `work-record-capture` and
  routed CLI source discovery/path classification through it. Codex and Pi
  remain native imports; normalized JSONL for other providers is now an
  explicit developer/test input gated by `CTX_PROVIDER_NORMALIZED_IMPORT_DEV=1`.
- 2026-06-25: Reclassified Claude, OpenCode, Antigravity, Gemini, Cursor,
  Copilot CLI, Factory AI Droid, and Amp as `detected_unsupported` in the
  provider matrix. Each has a concrete blocker reason. No provider was promoted
  without native persisted-history evidence.
- 2026-06-25: Removed the OpenRouter generated provider E2E Bazel target,
  Buildkite step, provider-live lane, and release-contract expectations.
  OpenRouter-generated histories are documented only as developer/static fixture
  drafting aid.
- 2026-06-25: Addressed reviewer blockers in public docs and release evidence:
  normalized provider JSONL is documented as env-gated developer input,
  detection-only providers are not described as importable support, Claude and
  Gemini release notes are blocked pending native importers, and OpenRouter
  helper scripts now use fixture-drafting language/env vars only.

## Decision Log

- Treat existing provider worktrees as integration candidates only. Nothing is
  accepted into the target branch unless it satisfies native persisted-history
  evidence and hermetic static fixture tests.
- Preserve normalized provider JSONL only as explicit development/test input,
  never as the public support mode for named providers.
- Do not implement Gemini native import in this slice even though upstream JSONL
  recording paths are provable; ctx still lacks a parser, sanitized native
  fixtures, and malformed/idempotency coverage for that format.
- Do not implement Copilot CLI native import in this slice even though official
  docs prove local session-state/session-store locations; ctx still lacks exact
  schema fixtures, a read-only parser, and redaction coverage.

## Validation Checklist

- [x] Relevant crate/unit tests for provider parsers and CLI provider behavior:
  focused `cargo test -p ctx ...` filters passed.
- [x] Fresh-home static fixture E2E:
  `bazel test //:fresh_home_e2e --config=ci` passed as part of
  `//:release_contract`.
- [x] Docs check:
  `bazel test //:docs_check --config=ci` passed as part of
  `//:release_contract`.
- [x] Package/content audit for forbidden public surfaces:
  `bazel test //:package_audit_fast --config=ci` and
  `//:package_audit_release` passed as part of validation.
- [x] Release contract:
  `bazel test //:release_contract --config=ci` passed, 28/28 targets.
- [x] `bazel query //...` passed and confirmed
  `provider_live_e2e_openrouter` is not present.
- [x] Final reviewer certification against the delegated prompt:
  reviewers certified the native-support boundary after stale docs/release
  wording was corrected.

## Handoff Notes

Current critical path is reviewer certification, commit, and push. Do not
promote any provider beyond Codex/Pi unless exact native persisted-history
format, static fixtures, parser tests, CLI discovery/import behavior, and
docs/matrix evidence all land together.
