# Work, ADE Extensibility, And Local Plugin Platform End-To-End Plan

## Purpose

This plan defines the remaining local-only implementation program required to
turn the current Work/ADE branch into a coherent, fully validated, hackable ctx
platform.

The product goal is:

- ctx is the hackable local ADE for coding agents.
- Work is the durable local record model behind the ADE and standalone capture.
- ACP is the public provider integration protocol.
- ctx plugins package/register providers and contribute commands, collectors,
  observers, Workbench panels, templates, and renderers.
- CRP remains an internal/native compatibility layer where useful; it is not the
  public protocol authors are asked to implement.
- The eventual harness starter kit is ACP-compatible and modular. It can use ctx
  primitives for shell, file, edit, sandbox, transcript, artifact, and Work
  capture without forcing developers into a ctx-specific agent protocol.

This plan explicitly excludes hosted services, team/enterprise product surfaces,
tenant administration, billing, hosted sync, pushing to remote `main`, opening a
PR, or releasing a production/stable build.

Network-adjacent features must stay passive/local by default. Provider
install/discovery metadata, auth/status surfaces, PR/contribution links, and
Buildkite readiness may read or display already-local data. Any command that
contacts a remote service must be optional, explicitly documented, non-required
for final done, and must not publish, release, push, open a PR, mutate hosted
state, or depend on hosted/team product infrastructure.

## Current Baseline

The current branch already contains the first Workbench composition slice:

- built-in Workbench template state and persistence;
- Classic, Kanban, Multipane, and Review templates;
- task-level Work projection from existing agent-work records;
- plugin runtime hardening for duplicate IDs and stdin BrokenPipe handling;
- safe relative path validation for Work artifacts/file endpoints;
- public decision docs around Work, ACP, plugins, and future harness primitives;
- `cargo-safe` and `check-local` wrappers for memory-capped local validation.

The branch is locally green for the earlier slice, including web gates, Buildkite
Bazel config/schema tests, and a full Rust workspace test through `cargo-safe`.
That does not mean the full platform is done. This document defines what remains.

## Operating Rules

1. Work locally on the manager branch/worktree unless explicitly creating
   parallel implementation worktrees.
2. Do not push to any remote and do not open a PR.
3. Commit as work lands. No giant dirty patch should accumulate.
4. Use manager-guided parallelism:
   - create separate worktrees for disjoint slices;
   - give subagents explicit file ownership;
   - require each subagent to run focused validation before handoff;
   - merge or cherry-pick passing changes back into the manager branch;
   - never allow two workers to own the same write set at the same time.
5. Keep heavy validation serialized and memory-capped. Rust workspace tests must
   use `core/scripts/dev/cargo-safe.sh` with explicit memory/job/thread limits.
6. Every feature slice must have implementation, tests, docs/examples when
   public-facing, and reviewer sign-off before it is considered complete.
7. Periodically run deep architecture review and deep SDLC review, even if tests
   pass.
8. Screenshot and manual UX validation are required for UI features. Automated
   tests are not enough.

## Architecture Decisions Already Settled

### Product Vocabulary

Use **Work** as the public product noun in UI, CLI, docs, and future API names.
The current `agent-work` schema/route/type names are compatibility names only.
If renamed, the rename must be one planned compatibility migration across:

- schemas;
- Rust types/routes;
- TypeScript types;
- CLI subcommands;
- import/export paths;
- docs;
- compatibility fixtures.

### Provider Protocol

ACP is the public provider integration protocol. A third-party coding agent
should implement ACP, then be packaged/registered in ctx as a plugin provider
contribution.

CRP may remain for native/runtime compatibility and internal richness, but ctx
should not market CRP as the protocol that external authors must implement.

### Plugin Versus Composability

Composability is the internal architecture. Workbench primitives such as task
lists, task cards, transcript panes, review surfaces, terminals, artifact
viewers, provider status, and Work detail must be reusable across templates.

Plugins are the packaging and contribution mechanism. A plugin contributes
composable pieces:

- commands;
- ACP providers;
- collectors/importers;
- observers;
- redaction/export processors;
- Workbench panels;
- Workbench templates;
- artifact/rendering extensions;
- future harness starter kit modules.

### `ctx work`

`ctx work` is public and documented. It is an advanced CLI surface, but it must
exist because it proves the ADE and standalone Work capture share one local
record model.

### Near-Term Wedge

Pursue ADE extensibility and Work capture in parallel. The public wedge is the
hackable ADE. Work capture is the substrate that makes extensions durable,
replayable, exportable, inspectable, and useful outside the GUI.

### Work Capture Source Of Truth

Work capture must be more than graph decoration. The local Work record model is
the source of truth for everything the ADE and `ctx work` need to inspect,
export, import, replay, review, or attribute.

First-class local Work records must cover:

- Work/task identity and lifecycle;
- sessions;
- runs/attempts;
- transcript messages/chunks;
- tool calls and tool results;
- artifacts, including screenshots and trace-like outputs;
- checks and evidence;
- local gates;
- change sets/diffs;
- PR/contribution links;
- manual attestations and reviewer notes;
- provenance and redaction metadata.

Local gates are not hosted policy. In this public local scope, a gate is a
recorded check/evidence checkpoint that may optionally block local UI actions
such as "mark ready", "merge", or "export without warning". Gates are local,
inspectable, bypassable by explicit user action when appropriate, and represented
as Work data. Hosted/team enforcement is out of scope.

### Mutation Boundaries

UI primitives and plugin UI contributions must not mutate core Work records
directly. They may call approved ctx actions with typed inputs. Those actions are
responsible for validation, provenance, redaction defaults, event emission, and
store writes.

## Prior Art To Study And Apply

The implementation should explicitly study and learn from:

- Vibe Kanban: agent task board, isolated workspaces, diff review, preview/dev
  server flows, and the "many agents in parallel" orchestration model.
  References: https://vibekanban.com/ and
  https://github.com/BloopAI/vibe-kanban
- tmux: pane splitting, resize behavior, layout switching, pane zoom,
  synchronize-panes, keyboard-driven navigation, mouse resizing, and robust
  layout persistence. Reference:
  https://man7.org/linux/man-pages/man1/tmux.1.html
- Zellij: tabs, named sessions, preconfigured layouts, floating/stacked panes,
  plugin architecture, status/help affordances, and discoverable keyboard
  workflows. Reference: https://zellij.dev/faq/
- Pi coding agent: modular, reloadable, user-owned customization ethos. Use this
  as inspiration for the future harness starter kit, but do not copy naming or
  create a ctx-specific competing agent protocol.

Prior-art research is not a one-time activity. Each UI/template slice must have
a short "what we borrowed / what we deliberately did not borrow" note in the
slice completion record.

## Feature Program

### Phase 0: Manager Branch Hygiene And Commit Discipline

Objective: make the current branch safe to extend.

Tasks:

1. Reconfirm owned worktree, branch, and status.
2. Split the existing dirty branch into readable commits:
   - docs/decision updates;
   - Workbench primitive/template implementation;
   - Work projection/schema/store hardening;
   - plugin runtime hardening;
   - validation wrappers;
   - tests.
3. Do not squash everything into one commit.
4. After each commit, run at least:
   - `git diff --check HEAD~1..HEAD`;
   - relevant focused tests for the touched area.
5. Maintain a local manager changelog in this exec-plan directory recording:
   - commit hash;
   - summary;
   - validation run;
   - reviewer/subagent sign-off.

Acceptance:

- branch has no uncommitted work except the next active slice;
- commit history is readable and bisectable;
- local changelog maps commits to validations and reviews.

### Phase 1: Work Namespace And Compatibility Plan

Objective: settle public Work naming without breaking compatibility.

Tasks:

1. Inventory all current `agent-work` public surfaces:
   - schemas;
   - HTTP routes;
   - Rust modules/types;
   - TypeScript types;
   - docs;
   - tests;
   - CLI affordances if present.
2. Decide the exact migration shape:
   - immediate rename to `work` with compatibility aliases; or
   - defer rename but add explicit deprecation/compatibility wrappers.
3. If implementing now, add:
   - `schemas/work/v1.schema.json` or equivalent;
   - compatibility route aliases from `agent-work` to `work`;
   - import/export compatibility fixtures;
   - docs that describe compatibility.
4. If deferring, add:
   - tracked TODO/ADR with blocked-on items;
   - tests that prevent new public `agent-work` copy from spreading.

Acceptance:

- public docs use Work consistently;
- any remaining `agent-work` usage is compatibility or internal-only;
- tests lock in whichever migration decision is chosen.

### Phase 2: Work Model Source Of Truth

Objective: make the local Work data model explicit enough that the ADE,
`ctx work`, import/export, plugins, and future harness primitives all target the
same record substrate.

Tasks:

1. Inventory current durable records and projections:
   - tasks;
   - sessions;
   - runs;
   - worktrees;
   - transcripts/messages;
   - tool calls;
   - artifacts;
   - checks/evidence;
   - gates;
   - change sets;
   - contributions/PR links;
   - manual notes/attestations;
   - provenance/redaction fields.
2. For each record family, define:
   - owner/source of truth;
   - schema version;
   - Rust store representation;
   - route/API surface;
   - TypeScript type/projection;
   - import/export shape;
   - redaction behavior;
   - ADE projection behavior;
   - compatibility path from current `agent-work` names.
3. Define storage semantics before implementation fan-out:
   - transaction boundaries for multi-record writes;
   - referential integrity between Work, sessions, runs, transcripts, artifacts,
     checks, gates, change sets, and contributions;
   - ID generation and ID stability rules;
   - record versioning and migration strategy;
   - optimistic concurrency or locking behavior;
   - corruption detection and recovery behavior;
   - large transcript indexing/search strategy;
   - artifact object indexing and checksum rules;
   - partial write rollback behavior;
   - import/export consistency snapshots.
4. Add missing records or compatibility wrappers where current data is only
   inferred from unrelated state.
5. Define local gate semantics:
   - gate status;
   - evidence/check references;
   - optional UI blocking behavior;
   - explicit bypass/override record;
   - no hosted/team enforcement in public local scope.
6. Ensure Work projections do not merely count links; they expose enough
   structured data for Review, Timeline, Artifact QA, and plugin panels.

Tests:

- schema tests for each first-class record family;
- Rust store tests for validation, ownership, and path safety;
- TypeScript projection tests for multi-session/multi-run/multi-PR Work;
- compatibility fixture tests for existing `agent-work` records;
- local gate tests covering pass/fail/warn/bypass states.
- transaction/rollback tests for multi-record writes;
- referential integrity tests;
- migration compatibility tests;
- concurrent writer or stale version tests where supported;
- corruption/partial-write recovery tests;
- large transcript/artifact indexing tests at practical local scale.

Acceptance:

- Work source-of-truth matrix is checked in;
- storage semantics are checked in and must land before broad parallel worker
  implementation begins;
- ADE and CLI can target the same record families;
- gates are local Work records, not hosted policy concepts;
- no new public feature invents a separate data model.

### Phase 3: `ctx work` CLI, Import, Export, Validate, Redact

Objective: make Work capture usable outside the ADE and prove it uses the same
local records.

CLI commands:

- `ctx work schema`
- `ctx work list`
- `ctx work show <work-id>`
- `ctx work capture <source|command>`
- `ctx work export <work-id|workspace|query>`
- `ctx work import <bundle|path>`
- `ctx work validate <bundle|path>`
- `ctx work redact <bundle|path>`
- `ctx work redaction-preview <bundle|path>`
- `ctx work inspect <bundle|path>`

Implementation requirements:

1. Use the same Rust store/schema model as the ADE.
2. Support local-only bundles first:
   - Work metadata;
   - sessions/runs;
   - transcript chunks/messages;
   - tool calls;
   - artifacts with safe relative paths;
   - checks/evidence;
   - change sets;
   - PR/contribution links;
   - provenance and redaction metadata.
3. Define a concrete bundle format:
   - bundle manifest version;
   - included schema versions;
   - object index;
   - per-object checksums;
   - total size and per-object size limits;
   - compression format and decompressed size limits;
   - no symlink extraction by default;
   - canonical safe relative paths only;
   - duplicate artifact path handling;
   - unknown schema version behavior;
   - dry-run diff output;
   - partial import rollback behavior.
4. Default export must omit or redact:
   - host absolute paths;
   - env vars/secrets;
   - raw transcript bodies unless explicitly included;
   - private token-like values;
   - local user/home path leakage.
5. Explicit flags must control inclusion:
   - `--include-transcripts`;
   - `--include-artifacts`;
   - `--include-host-paths` only if there is a defensible use case;
   - `--redaction-profile`.
6. Import must support:
   - old control-plane data where locally available;
   - future old session import;
   - id remapping;
   - duplicate detection;
   - dry-run validation.
7. Import must reject or safely handle:
   - path traversal;
   - absolute paths;
   - symlinks;
   - duplicate artifact paths;
   - compression bombs;
   - checksum mismatches;
   - max-size violations;
   - unknown required schema versions;
   - corrupted manifests;
   - partially written bundles.
8. The ADE must be able to show imported Work records.

Tests:

- schema validation fixtures;
- redaction fixtures with adversarial secrets/paths;
- round-trip export/import tests;
- import idempotency tests;
- corrupted bundle tests;
- checksum, max-size, symlink, duplicate path, compression bomb, unknown schema,
  and partial rollback tests;
- CLI snapshot tests;
- ADE projection tests over imported records.

Acceptance:

- CLI is documented and discoverable;
- bundle manifest format is documented;
- same Work record can be created/imported/exported and viewed in ADE;
- redaction defaults are safe;
- all fixtures are versioned and stable.

### Phase 4: ACP Provider Contract And Conformance

Objective: make "implement ACP, then package/register as a ctx plugin" real
instead of merely documented.

Tasks:

1. Declare the ACP version/commit/spec target for this branch. If ACP evolves
   during implementation, update the target deliberately and rerun conformance
   tests.
2. Document the provider plugin contract:
   - plugin manifest contribution shape;
   - ACP command/entrypoint declaration;
   - provider ID ownership and collision rules;
   - install/discovery metadata;
   - auth/status capability mapping;
   - command catalog passthrough;
   - diagnostics.
3. Add at least one ACP provider fixture:
   - fake/demo ACP-compatible provider;
   - deterministic transcript/tool-call behavior;
   - failure mode fixture.
4. Add conformance-style tests:
   - provider appears in catalog;
   - provider status/diagnostics surface correctly;
   - streaming output maps correctly into session and Work records;
   - cancellation is routed and normalized;
   - errors normalize into diagnostics and Work events;
   - auth/status lifecycle is represented consistently;
   - tool-call/result pairs map to Work tool-call records;
   - backpressure and output caps are enforced;
   - session resume/reload behavior is defined and tested;
   - provider process death is detected and surfaced;
   - provider command conflicts are namespaced/source-labeled;
   - provider removal/reload cleans up ownership without breaking active
     sessions;
   - ACP messages normalize into Work transcript/tool-call records.
5. Define CRP boundaries in docs:
   - internal/native compatibility;
   - not the public extension target;
   - bridge behavior where ACP provider output enters ctx runtime.

Acceptance:

- ACP provider plugin integration has executable fixtures, not only docs;
- ACP version target and conformance matrix are documented;
- streaming, cancellation, error normalization, auth/status lifecycle,
  tool-call/result mapping, backpressure/output caps, session resume/reload, and
  provider process death are tested or explicitly deferred with user approval;
- provider ID collisions and ownership cleanup are tested;
- ACP-to-Work capture path is covered by tests;
- CRP remains out of the external integration story.

### Phase 5: Plugin Manifest, Typed SDK, And Example Plugins

Objective: turn plugin contributions into a typed, documented public surface
without prematurely overcommitting to a huge SDK.

Package shape:

- repo-local TypeScript package first, publishable later;
- likely name: `@ctx/plugin-sdk`;
- no npm publish required in this local program.

SDK minimum:

- `defineCtxPlugin(...)`;
- manifest type definitions;
- manifest validation helper;
- contribution builders for:
  - commands;
  - ACP providers;
  - collectors/importers;
  - observers;
  - redaction/export processors;
  - Workbench panels;
  - Workbench templates;
  - Workbench card renderers;
  - Workbench detail sections;
  - Workbench review sections;
  - Workbench toolbar actions;
  - artifact renderers;
- version/capability declarations;
- test helper for validating a plugin fixture.

Any contribution type declared in the public manifest but not included in the
first SDK must be explicitly marked deferred in the SDK docs, schema comments,
tests, and completion record. Silent partial support is not acceptable.

Example plugins:

1. Review checklist panel:
   - declarative panel;
   - reads current Work/task state;
   - contributes command to generate checklist from current diff.
2. ACP provider plugin:
   - registers a fake/demo ACP provider;
   - declares install/discovery metadata;
   - appears in provider picker.
3. Work importer plugin:
   - imports a simple transcript fixture;
   - writes Work events;
   - can be run through `ctx work import`.
4. Artifact renderer plugin:
   - renders screenshots or Playwright traces using a declarative renderer.

Acceptance:

- examples compile/test locally;
- plugin schema and SDK types agree;
- invalid plugin examples fail with actionable diagnostics;
- docs explain local development and future publishability.

### Phase 6: Declarative Workbench Contributions

Objective: allow plugins to contribute composable UI without arbitrary code
execution as the default.

Contribution types:

- `workbench.panels`;
- `workbench.templates`;
- `workbench.card_renderers`;
- `workbench.detail_sections`;
- `workbench.artifact_renderers`;
- `workbench.review_sections`;
- `workbench.toolbar_actions`.

Declarative first:

- ctx owns layout, state subscriptions, permissions, styling, empty states, and
  lifecycle;
- plugins declare what they need and which renderer/data binding they use;
- arbitrary webview/React UI is deferred or gated behind explicit capability.

Typed data/action contract:

- data bindings must be explicit and versioned, for example:
  - current Work summary;
  - current task/session;
  - transcript slice;
  - tool-call list;
  - artifact list;
  - check/gate state;
  - change-set/diff summary;
  - provider status;
- subscriptions must define lifecycle:
  - initial loading state;
  - incremental update behavior;
  - empty state;
  - error state;
  - stale data behavior after reload;
  - unmount cleanup;
- actions must use stable action IDs and typed inputs:
  - start task;
  - focus Work/task/session;
  - run ctx command;
  - run plugin command;
  - export/redact Work;
  - attach artifact;
  - add manual note/attestation;
  - update local gate state;
- mutation permissions must be declared by capability and enforced through ctx
  action handlers. Plugin UI must not write store records directly.
- contribution versions must have compatibility behavior for older persisted
  layouts and removed plugin contributions.

Concrete examples and expected mode:

1. Review checklist panel: declarative.
2. Custom triage Kanban template: declarative.
3. Diff summary command and detail section: declarative command plus detail
   renderer.
4. ACP provider registration: non-UI provider contribution.
5. Old transcript importer: collector/importer contribution.
6. Screenshot gallery renderer: declarative renderer first.
7. Playwright trace inspector: likely arbitrary webview later.
8. Full custom debugger/timeline: likely arbitrary webview later.
9. Whole-workbench replacement: not first pass; compose templates instead.

Acceptance:

- plugin-contributed panels/templates appear beside built-ins;
- data bindings, subscription lifecycle, loading/error states, action IDs,
  mutation permissions, and version compatibility are documented and tested;
- source labels identify plugin contributions;
- untrusted/invalid contributions are rejected safely;
- plugin UI cannot mutate core Work records except through approved actions;
- templates remain usable on narrow/mobile screens with fallback behavior.

### Phase 7: Source-Labeled Commands And Provider Command Passthrough

Objective: stop conflating provider slash commands with ctx/plugin commands.

Tasks:

1. Build a unified command catalog with source labels:
   - provider;
   - ctx;
   - plugin;
   - workspace/local.
2. Preserve provider-owned commands from ACP/provider catalogs.
3. Prevent silent shadowing:
   - duplicate IDs must show source or namespace;
   - plugin commands cannot override provider commands;
   - conflicts produce diagnostics.
4. Route command execution to the correct owner:
   - provider command goes to provider/session;
   - plugin command goes to plugin process/entrypoint;
   - ctx command goes to internal action.
5. Surface command availability by context:
   - active session;
   - selected Work;
   - selected diff;
   - selected artifact;
   - workspace.

Tests:

- projection tests for mixed provider/plugin/ctx commands;
- conflict tests;
- execution route tests;
- UI render tests showing source labels;
- slash command compatibility tests.

Acceptance:

- users can tell where a command comes from;
- commands execute through the correct owner;
- duplicate/conflicting commands are safe and explainable.

### Phase 8: Hot Reload And Plugin Dev Loop

Objective: make customization fast without killing active daemon-owned sessions.

UI hot reload:

- plugin manifest changes reload contribution registry;
- panels/templates/renderers remount or refresh;
- active task/session remains because daemon owns session state;
- UI subscriptions reconnect;
- layout and selection persist where compatible.

Daemon-side hot reload:

- re-read manifests;
- validate contributions;
- restart plugin subprocesses when needed;
- define behavior for in-flight plugin commands:
  - command keeps running under the old plugin revision; or
  - command is cancelled with an explicit diagnostic if the entrypoint is
    removed;
- preserve collector cursors/checkpoints;
- update registry generation ID;
- notify UI subscribers;
- show diagnostics.

Removal/error semantics:

- removed panels/templates disappear from the contribution catalog;
- persisted plugin template state migrates to Classic or a declared fallback;
- removed provider contributions stop being selectable for new sessions;
- active sessions owned by a removed provider keep their daemon-owned state and
  show "provider unavailable" for new turns until the provider returns or the
  user switches provider;
- bad manifest reload keeps the last good plugin revision active where safe and
  surfaces diagnostics;
- bad manifest reload must not corrupt stored Work records or Workbench layout.

Explicit restart/apply required for:

- provider runtime command changes for already-running sessions;
- active agent environment variable changes;
- sandbox/container policy changes;
- auth state changes;
- model/runtime changes affecting active agent behavior.

CLI/dev commands:

- `ctx plugin list`
- `ctx plugin validate <path>`
- `ctx plugin reload [id]`
- `ctx plugin dev <path>`
- `ctx plugin logs <id>`

Acceptance:

- changing a panel/template manifest updates UI without killing sessions;
- add/change/remove flows are covered by tests;
- bad manifest recovery is tested;
- active command behavior across reload is tested;
- provider ownership cleanup is tested;
- persisted plugin template fallback is tested;
- changing a collector restarts from checkpoint or reports why it cannot;
- changing provider config prompts/applies only to future sessions unless user
  explicitly restarts;
- diagnostics are visible in CLI and ADE.

### Phase 9: Workbench Template Product Polish

Objective: make built-in templates genuinely good, not just renderable.

General requirements:

- stable dimensions;
- no text overflow or overlap;
- responsive desktop and mobile fallbacks;
- keyboard navigation;
- accessible focus states;
- source/status affordances;
- predictable empty/loading/error states;
- no nested-card clutter;
- restrained work-focused visual design;
- no layout shift on hover or dynamic content;
- performance remains acceptable with many tasks/sessions/artifacts.

Classic:

- unchanged default behavior;
- no regression in existing task/session flow;
- all existing commands/menus remain reachable.

Kanban:

- lanes are meaningful for coding-agent work, not generic toy lanes;
- supports active/running/review/blocked/done or equivalent;
- cards show provider, branch/worktree, status, PR/change-set hints, last
  activity, and attention state;
- selected task detail is useful without leaving board;
- drag/drop is considered and either implemented or explicitly deferred with a
  keyboard/menu alternative;
- compare against Vibe Kanban and document what ctx does differently.

Multipane:

- split/resize/focus feels closer to tmux/Zellij than a static grid;
- supports horizontal/vertical split;
- supports reset/even layout;
- supports zoom/focus pane if feasible;
- drag handles are discoverable and accessible;
- persisted split tree survives reload;
- invalid/stale pane state recovers gracefully;
- terminal pane behaves correctly inside layout;
- future plugin panes fit the same model.

Review:

- focuses on diff, Work summary, evidence, artifacts, checks, transcript, and
  PR/contribution links;
- supports multi-session/multi-agent Work records;
- makes "what changed, why, evidence, risk, next action" scannable;
- does not become a marketing dashboard.

Additional templates to consider:

- Timeline: chronological transcript/tool-call/artifact/check view.
- Review War Room: diff + findings + transcript + test artifacts.
- Terminal Ops: tmux/Zellij-inspired terminal/work panes.
- Artifact QA: screenshot/browser/dev-server/test artifact centric.
- Provider Lab: compare provider runs on same task.

Acceptance:

- each built-in template has automated tests and screenshot/manual review;
- at least one new creatively considered template is either implemented or
  explicitly deferred with rationale;
- reviewers approve UX quality from actual screenshots, not only code.

### Phase 10: Screenshot, Manual UX, And Visual Artifact Review

Objective: make UI review concrete and repeatable.

Required viewports:

- desktop wide;
- laptop width;
- tablet/narrow;
- mobile fallback where supported;
- high-density task list scenario;
- empty workspace;
- active running sessions;
- completed/reviewable task;
- error/diagnostic state.

Required artifacts:

- screenshots for Classic, Kanban, Multipane, Review;
- screenshots before and after changing templates;
- screenshots after reload showing persistence;
- screenshots while resizing Multipane;
- screenshots of plugin-contributed panel/template once implemented;
- screenshots of plugin command surfaces with source labels;
- screenshots of plugin provider failure/diagnostics;
- screenshots of hot reload add/change/remove states;
- screenshots of import/export errors and redaction preview;
- short video or screenshot sequence for drag/resize/snap behavior if available.

Review process:

1. Start local app/dev server.
2. Seed deterministic demo data.
3. Use Playwright or ctx browser tooling to capture screenshots.
4. Attach artifacts under `.ctx/attachments` or a documented local artifact path.
5. Primary implementer manually opens/views images.
6. UI reviewer subagent manually reviews images and reports:
   - overlap;
   - clipping;
   - weak hierarchy;
   - broken responsiveness;
   - confusing affordances;
   - inaccessible focus/contrast;
   - missing empty/error states.
7. Fix issues and retake screenshots.

Acceptance:

- screenshots are reviewed by at least two agents, including one adversarial UI
  reviewer;
- all actionable visual issues are fixed or explicitly deferred with rationale;
- final screenshot set is listed in the completion record.

### Phase 11: Test Coverage And Adversarial Gap Review

Objective: make the platform robust at every layer.

Coverage areas:

- schema validation;
- Rust store validation and redaction;
- Rust plugin inventory/runtime;
- CLI command behavior;
- import/export round trips;
- Work projection;
- TypeScript SDK typing and manifest validation;
- UI primitive behavior;
- template persistence;
- plugin panel/template contribution projection;
- command catalog conflict/routing;
- hot reload registry generation;
- Playwright/E2E flows;
- accessibility-focused tests where feasible;
- screenshot regression or at least screenshot review artifacts.

Required reviewers:

- implementation reviewer;
- adversarial test coverage reviewer;
- security/privacy reviewer;
- architecture reviewer;
- SDLC/process reviewer;
- final done-ness reviewer.

Coverage reviewer prompt must ask:

- What user-visible behavior is untested?
- What boundary condition can corrupt Work data?
- What plugin input can crash or escape capabilities?
- What import/export case leaks private data?
- What UI state can become stale after reload?
- What command conflict can route to the wrong owner?
- What Buildkite/local gap could let a failure escape?

Acceptance:

- every feature slice has focused tests;
- cross-slice E2E tests cover the happy path and at least one failure path;
- adversarial reviewer signs off that remaining gaps are acceptable.

### Phase 12: Security, Privacy, And Capability Review

Objective: avoid turning extensibility into an unsafe arbitrary execution path.

Threat model:

- local plugins are powerful local code and should be treated as trusted by
  default only when installed from explicit plugin roots;
- dev-mode plugins may execute host processes, but this must be visible and
  source-labeled;
- production/default UX must avoid silently running arbitrary host executables
  from untrusted paths;
- plugin process stdout/stderr, env, cwd, and artifact paths are possible secret
  leakage channels;
- plugin provider IDs and command IDs are authority-bearing names and must not
  be silently hijackable;
- import/export bundles are untrusted input even when local.

Review areas:

- plugin manifest path handling;
- command execution cwd/env handling;
- trusted plugin roots and root escape prevention;
- command path rules for absolute and relative executables;
- env allowlist/redaction behavior;
- command timeout and output caps;
- safe relative path enforcement;
- artifact path traversal;
- import bundle traversal;
- transcript/redaction defaults;
- secrets and env var leakage;
- provider config changes;
- plugin identity and duplicate IDs;
- provider ID ownership/collision;
- command ID ownership/collision;
- capability declarations;
- user consent requirements for executing plugin entrypoints;
- arbitrary UI/webview restrictions;
- cross-workspace data isolation;
- daemon API mutation boundaries.

Acceptance:

- security reviewer signs off;
- exploit-style tests exist for high-risk areas;
- plugin root escape, env leakage, provider ID collision, redaction, command
  timeout/output cap, and path traversal tests pass;
- public docs are honest about local plugin trust model.

### Phase 13: Architecture Review

Objective: periodically verify the program is still coherent.

Architecture review checkpoints:

1. After Work CLI/import/export is designed.
2. After plugin SDK/contribution schema is implemented.
3. After hot reload works.
4. Before final done-ness review.

Questions:

- Are ADE and Work capture still one system?
- Is ACP clearly the public provider protocol?
- Is CRP contained to internal/native compatibility?
- Are plugins contributing composable primitives rather than replacing the app?
- Are daemon-owned session lifecycles protected from UI reload?
- Are Work records durable, versioned, importable, exportable, and redaction
  aware?
- Are hosted/team concerns excluded from public local code?
- Would this architecture still support a future modular harness starter kit?

Acceptance:

- architecture reviewer provides written sign-off at each checkpoint;
- any unresolved disagreement is recorded with owner and decision.

### Phase 14: SDLC And Agent Workflow Review

Objective: optimize the process for agent-driven development speed, correctness,
and stability.

Review areas:

- worktree ownership and merge discipline;
- commit granularity;
- validation matrix;
- subagent instructions;
- artifact collection;
- branch dirt hygiene;
- Buildkite/local parity;
- flaky test handling;
- resource caps;
- regression triage;
- plan conformance.

Acceptance:

- SDLC reviewer signs off before final phase;
- process defects are fixed, not only noted;
- the final completion record lists commands, artifacts, subagents, and commits.

### Phase 15: Buildkite, Bazel, Artifacts, And Full Local Validation

Objective: reach the same confidence as CI without pushing or releasing.

Local commands must include:

```bash
pnpm -C core/apps/web typecheck
pnpm -C core/apps/web lint
pnpm -C core/apps/web test
pnpm -C core/apps/web build
cargo fmt --manifest-path core/Cargo.toml --all -- --check
core/scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --workspace --locked
.buildkite/validate-pipeline.sh
.buildkite/run-bazel.sh test //:buildkite_config_test //:schemas
git diff --check
```

Additional commands must be added as implementation creates them:

- CLI integration tests;
- Playwright/E2E tests;
- plugin SDK package tests;
- docs/schema generation checks;
- artifact build/package checks;
- any Buildkite taxonomy/shift-left tests added by the branch.

Buildkite requirement:

- The branch must be ready to pass Buildkite.
- Run local Buildkite validation:
  - `.buildkite/validate-pipeline.sh`;
  - `buildkite-agent pipeline upload --dry-run .buildkite/pipeline.yml` if the
    local agent is installed and supports dry-run without remote push;
  - direct shifted-left commands for every Buildkite taxonomy bucket touched by
    the branch.
- If a local Buildkite agent is available and configured without pushing, run the
  relevant pipeline locally or explain the exact local equivalent.
- If Buildkite remote execution requires a pushed branch, do not push; instead
  run the full local shifted-left matrix and record that remote Buildkite was
  intentionally excluded by scope.

Artifact requirement:

- all build artifacts required by the local CI matrix are built;
- no production release or stable publish is performed;
- any canary-like local artifact is clearly local-only and not distributed.

Acceptance:

- every required command exits 0;
- failures are fixed and rerun;
- ignored/skipped tests are listed with rationale;
- no heavy validation processes remain running.

## Parallel Subagent Plan

### Manager Responsibilities

The primary session owns:

- architecture decisions;
- plan updates;
- branch hygiene;
- conflict resolution;
- final integration;
- final validation;
- final done-ness review request.

### Worker Types

Use disjoint worktrees for:

1. Work CLI/import/export.
2. Plugin SDK/manifest/examples.
3. Declarative Workbench contributions.
4. Command catalog/source labels.
5. Hot reload/dev loop.
6. UI template polish/screenshot automation.
7. Test coverage expansion.
8. Security hardening.
9. Docs/examples.

Hard prerequisite before broad parallelism:

- Phase 1 and Phase 2 contract files must land in the manager branch first.
- The plugin capability/contribution model and ACP conformance matrix must land
  in the manager branch before plugin/provider workers branch from it.
- Every worker must base on the manager commit that contains those contracts.
- The manager records that base commit in `manager_changelog.md`.

Each worker must receive:

- exact branch/worktree path;
- clean-base requirement before worktree creation;
- write ownership;
- files/modules not to touch;
- expected tests;
- handoff format;
- reminder not to revert others' edits.

Integration rules:

- manager-owned schema, namespace, and capability changes must land before
  workers build on them;
- workers must not independently rename `agent-work` to `work`;
- workers must not independently change plugin trust/capability semantics;
- workers must return:
  - base commit / merge base;
  - files touched;
  - exact diff stat;
  - invariants changed;
  - tests run;
  - expected conflicts;
  - residual risks;
  - integration notes;
- manager merges one phase at a time and reruns focused tests after each merge.

### Review Agents

Required review agents:

- Code quality reviewer.
- Security/privacy reviewer.
- Architecture reviewer.
- Test coverage reviewer.
- UI/visual reviewer.
- SDLC/process reviewer.
- Done-ness reviewer.

Reviewers should be adversarial and should not implement fixes unless assigned a
separate worker role.

## Completion Records

Maintain completion records in this exec-plan directory:

- `manager_changelog.md`;
- `validation_log.md`;
- `architecture_reviews.md`;
- `sdlc_reviews.md`;
- `ui_artifact_reviews.md`;
- `test_coverage_reviews.md`;
- `security_reviews.md`;
- `done_ness_review.md`.

Each record must include:

- date/time;
- branch/worktree;
- commit(s);
- files touched;
- commands run;
- screenshots/artifacts if applicable;
- reviewer findings;
- fixes applied;
- remaining accepted risks.

## Final Done Condition

The program is not complete until all of the following are true:

1. All feature phases above are implemented or explicitly deferred in writing
   with user-approved rationale.
2. Public docs describe:
   - Work model and `ctx work`;
   - plugin manifest and SDK;
   - ACP provider plugin integration;
   - Workbench panels/templates/renderers;
   - hot reload/dev loop;
   - import/export/redaction;
   - future ACP-compatible harness starter kit.
3. Work/ADE data is unified:
   - ADE uses the same local Work records as `ctx work`;
   - `ctx work` supports schema, validate, export, import, redaction preview,
     inspect, and idempotent re-import fixtures;
   - import/export round trips are tested;
   - Work records can be viewed in the ADE after CLI import;
   - session/run/change-set/artifact/check/gate/contribution records have
     store, route, schema, TypeScript, and UI projection tests where applicable;
   - transcript/tool-call/artifact/check/gate/change-set/PR data is versioned
     and validated.
4. Plugin extensibility is real:
   - typed SDK exists locally;
   - example plugins cover command, provider registration, UI contribution, hot
     reload, diagnostics, and failure behavior;
   - declarative panels/templates/renderers are usable;
   - ACP provider plugin contribution is documented and tested;
   - ACP conformance fixture exists or any unimplemented ACP surface is
     explicitly user-approved as deferred;
   - source-labeled commands prevent silent conflicts.
5. Hot reload works:
   - UI extensions reload without killing daemon-owned active sessions;
   - daemon plugin contributions reload or restart safely;
   - runtime changes that require restart are explicit.
   - add/change/remove, bad manifest recovery, active command behavior, provider
     ownership cleanup, and persisted plugin template fallback are tested.
6. Built-in Workbench templates are production-quality locally:
   - Classic has no behavior regression;
   - Kanban is useful and visually polished;
   - Multipane supports robust split/focus/resize/persistence behavior;
   - Review supports serious diff/evidence/transcript review;
   - additional template ideas have been evaluated and at least one is either
     implemented or deliberately deferred.
7. Screenshot/manual UX validation is complete:
   - required screenshots/videos are captured;
   - screenshot set includes template layouts, plugin command surfaces, plugin
     provider diagnostics, hot reload states, import/export errors, and redaction
     preview;
   - primary session manually views them;
   - UI reviewer subagent reviews them;
   - all actionable visual issues are fixed or recorded with rationale.
8. Test coverage is strong:
   - unit/integration/E2E tests cover every feature slice;
   - adversarial coverage reviewer signs off;
   - coverage gaps are fixed or user-approved.
9. Code quality, security, architecture, and SDLC reviews pass:
   - all actionable findings are fixed;
   - accepted risks are documented.
   - security review covers path traversal, env leakage, plugin root escape,
     provider ID collision, redaction, command timeout/output caps, and command
     execution consent.
10. Commit history is clean and reviewable:
    - no giant dirty patch;
    - commits map to validation records;
    - final `git status --short` only shows intentional uncommitted artifacts if
      explicitly documented, otherwise clean.
11. Full local validation passes:
    - web typecheck/lint/test/build;
    - Rust fmt;
    - full Rust workspace tests through memory-capped `cargo-safe`;
    - Buildkite pipeline validation;
    - Bazel shifted-left tests;
    - CLI/plugin/E2E/docs/schema/artifact tests added by this program;
    - `git diff --check`;
    - no leftover heavy processes.
12. Buildkite readiness is demonstrated:
    - remote Buildkite is not required if it would require pushing;
    - local shifted-left Buildkite/Bazel matrix and all equivalent gates pass;
    - if local Buildkite agent execution is available without remote push, it is
      run and passes.
13. A dedicated done-ness subagent reviews:
    - this plan;
    - completion records;
    - final git history/status;
    - validation output;
    - screenshot artifacts;
    - review sign-offs;
    - exclusions.
    - confirms no hosted/team/enterprise service work, remote push, remote main
      merge, PR opening, or production release slipped into scope.

The primary session must not send a final "done" response for the full program
until the done-ness subagent explicitly says the program satisfies the final done
condition. If the done-ness reviewer finds gaps, fix them and rerun the relevant
validation/review loop.
