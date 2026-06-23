<p align="center">
  <img src="assets/readme/work-record-banner.png" alt="ctx records agent work" />
</p>

ctx records agent work as durable local **work records** that can be searched,
reviewed, exported, and attached to pull requests through local CLI workflows.

This branch is the public local-first ctx `0.1.0` candidate preview. It
focuses on a source-build local product: work records, command evidence,
pull request links, search, reports, agent-readable context output, JSON
export/import, a static React/Vite dashboard, capture spool import, provider
fixture import, gated Codex prompt-history import, explicit Pi session import,
VCS/PR inspection, local Git/jj/gh command shims, and local storage validation.

The launch scope is intentionally conservative. Passive capture is shipped for
local Git/jj/gh command activity through ctx-owned shims, while provider-native
Codex/Claude/Pi/OpenCode/Antigravity/Gemini/Cursor hooks remain explicit
documented limitations. Hosted sync, public installer URLs, and production
hosted/team surfaces are not claimed in this branch.

This branch is not the ctx ADE. It does not document or ship the ADE workbench,
container runtime, hosted team product, `ctx.rs` cutover, or production
`ctx.rs/install` flow.

## Current Status

Implemented in this branch:

- create local work records with title, body, tags, kind, optional workspace,
  timestamps, and id;
- capture command evidence when commands are run through `ctx evidence run`;
- install local reversible Git/jj/gh wrapper shims that spool command evidence;
- validate/normalize pull request URLs with `ctx pr parse`, then store one
  local pull request URL string on a record with `ctx link-pr`;
- list, show, search, and render context for local records;
- generate text or JSON reports from recent records and evidence;
- export a static local React/Vite dashboard with local assets only;
- export/import ctx JSON archives;
- import pending local capture spool JSONL files;
- import normalized Codex, Claude, Pi, OpenCode, Antigravity, Gemini, and
  Cursor provider fixture JSONL into local summary records and rich capture
  data;
- import Codex prompt-history JSONL as prompt-only `summary_only` events when
  the user provides an explicit input path;
- import Pi session JSONL as explicit `imported` provider events when the user
  provides an explicit input path;
- automatically import pending capture spool files before normal work views;
- inspect Git/jj workspace metadata and parse GitHub/GitLab pull request URLs;
- validate and repair failed capture imports;
- uninstall shims, shell integration, and any optional service while keeping
  recorded data unless deletion is explicitly requested.

Explicit launch boundaries:

- passive provider hooks or shell hooks beyond the local Git/jj/gh wrapper
  shims;
- unbounded automatic scanning of provider history directories;
- Claude/OpenCode/Antigravity/Gemini/Cursor native provider-history import
  beyond normalized fixture JSONL;
- hosted sync, hosted sharing, accounts, team policy, hosted dashboards,
  organization analytics, or hosted retention controls;
- live public installer URLs for this branch;
- hosted publish/sync commands are not shipped; `ctx publish pr-comment` is
  local CLI-driven GitHub PR comment publishing through `gh`, not hosted sync.

The implemented CLI now uses root-level ctx work records commands. The older
`ctx workspace ...` and `ctx work ...` forms remain as hidden compatibility
aliases for the current local behavior.

## Install Or Run

Public installer URLs are not documented as live for this branch yet. Build or
install from this checkout:

```bash
cargo build -p ctx
cargo install --path crates/ctx-cli
```

You can also run commands from source:

```bash
cargo run -p ctx -- status
cargo run -p ctx -- list
```

## Quick Start

Create the local ctx store:

```bash
ctx setup
ctx status
```

By default, `ctx setup` creates or updates `~/.ctx`, installs Git/jj/gh capture
shims under `~/.ctx/shims`, enables future shells with a managed shell rc block
when that is safe, imports known supported provider history, and starts or
opens the local dashboard when the session looks interactive. It should ask
only on real ambiguity or failure. Use `--no-open`, `--no-import`,
`--no-shell-update`, `--service`, `--yes`, or `--dry-run` to constrain that
first-run behavior.

No persistent service is installed by default. Recording works through local
commands and shims without a daemon. `ctx setup --service` opts into the
background service path.

Create a work record:

```bash
ctx record \
  --title "fix checkout retry handling" \
  --body "Investigate flaky checkout retries and make retry behavior deterministic." \
  --tag checkout \
  --tag retry \
  --kind task \
  --json
```

Capture command evidence:

```bash
ctx evidence run --record <record-id> cargo test -p checkout
```

Optionally capture local Git/jj/GitHub CLI commands without repo hooks:

```bash
ctx shim install --dir .ctx-shims
eval "$(ctx shim env --dir .ctx-shims)"
git status
ctx capture import
```

Import the provider history that this branch can prove today:

```bash
ctx capture import-local-providers --json
ctx capture import-codex-history --input ~/.codex/history.jsonl --json
```

Validate and link a pull request URL locally:

```bash
ctx pr parse https://github.com/example/project/pull/42 --json
ctx link-pr <record-id> https://github.com/example/project/pull/42
```

Review and search:

```bash
ctx list
ctx show <record-id>
ctx search checkout
ctx context checkout
ctx report
ctx dashboard
```

`ctx dashboard` starts or reuses a small localhost server and opens the
dashboard when the environment supports it. In SSH, CI, other headless
sessions, or with `--no-open`, it prints the URL and command instead of opening
a browser. For a static review artifact, use `ctx dashboard export --output
./ctx-dashboard`.

Preview a pull request comment locally before publishing through `gh`:

```bash
ctx publish pr-comment <record-id> --dry-run
```

Inspect local repository metadata or parse a pull request URL:

```bash
ctx vcs inspect --json
ctx pr parse https://github.com/example/project/pull/42 --json
```

Move records between machines with ctx JSON archives:

```bash
ctx export --output ctx-records.json
ctx import --input ctx-records.json
```

`ctx import` imports ctx archive JSON only. It does not import existing
local agent history from provider transcript directories.

Import a normalized provider fixture:

```bash
ctx capture import-provider --provider codex --input tests/fixtures/provider/codex.jsonl --json
```

Provider fixture import currently supports `codex`, `claude`, `pi`,
`opencode`, `antigravity`, `gemini`, and `cursor` fixture JSONL. It creates a
summary work record and provider event, message, and tool-call fixture views
for new imported sessions/events so the content appears in search, context,
report, and dashboard output.

Import Codex prompt history explicitly when a local `history.jsonl` file exists:

```bash
ctx capture import-codex-history --input ~/.codex/history.jsonl --json
```

This path is prompt-log only. It marks imported rows as `summary_only` and does
not claim assistant replies, tool calls, command output, artifacts, or child
sessions. See [docs/provider-support.md](docs/provider-support.md) for the
provider support matrix and current E2E blockers.

Import a Pi session file explicitly when a local session JSONL file exists:

```bash
ctx capture import-pi-session --input ~/.pi/agent/sessions/<session>.jsonl --json
```

This path preserves Pi message entry ids and parent ids in metadata, but does
not convert message branches into ctx subagent edges or expand raw image blocks
into artifacts.

Import pending local capture spool files:

```bash
ctx capture import --json
```

The capture importer reads JSONL envelope files from the local ctx spool. The
optional Git/jj/gh wrapper shims can write these envelopes for local
command-line activity. Provider-native shell hooks are not implemented in this
branch. The provider-history paths are explicit local Codex prompt-history JSONL
import and explicit Pi session JSONL import, with the limitations described
above.

## Docs Map

Use these docs together when reviewing the `0.1.0` candidate wording and scope:

- [docs/getting-started.md](docs/getting-started.md): source install, setup,
  import, capture, search, report, dashboard export, and PR publish dry-run.
- [docs/work-model.md](docs/work-model.md): what ctx records, how evidence and
  capture spool data fit together, and the local-first product boundary.
- [docs/provider-support.md](docs/provider-support.md): provider support
  taxonomy, current candidate matrix, and wording rules.
- [docs/privacy-storage.md](docs/privacy-storage.md): privacy defaults, raw
  transcript posture, storage model, and sensitive-data handling.
- [docs/release-install.md](docs/release-install.md): honest installer contract
  language and security constraints.
- [docs/hosted-sync-roadmap.md](docs/hosted-sync-roadmap.md): future hosted
  sync direction without over-claiming shipped scope.
- [docs/troubleshooting.md](docs/troubleshooting.md): status, doctor, repair,
  validate, shim activation, and provider import triage.

## Work Records Model

A work record is the durable history for one unit of agent-assisted work. The
current implementation stores and reports:

- id;
- title;
- body;
- kind;
- tags;
- optional workspace path;
- optional pull request URL;
- created and updated timestamps;
- command evidence captured by `ctx evidence run` or imported from ctx-owned
  Git/jj/gh command shims;
- provider fixture sessions, events, messages, tool-call records, source
  cursors, fidelity labels, and parent/child relationships when present in the
  normalized input;
- Codex prompt-history rows imported with `summary_only` fidelity;
- Pi session rows imported with `imported` fidelity;
- evidence freshness metadata bound to the observed Git or jj state;
- pull request links with typed metadata when parsed or imported;
- share-safe report/dashboard DTOs with redacted command previews, artifacts,
  tags, and privacy summaries.

Provider-native transcript capture remains outside this launch scope unless the
provider support matrix documents it as implemented.

## CLI

The current command groups are:

```bash
ctx setup
ctx status [--json]
ctx dashboard [--no-open]
ctx uninstall [--delete-data] [--yes]
ctx service install
ctx service status
ctx service uninstall

ctx schema
ctx record --title "task title" --body "prompt or note" --kind task
ctx list
ctx show <record-id>
ctx search <query>
ctx context [query]
ctx report
ctx dashboard [--no-open]
ctx dashboard export --output <dir>
ctx evidence run [--record <record-id>] <command> [args...]
ctx shim install --dir <dir>
ctx shim env --dir <dir>
ctx shim uninstall --dir <dir>
ctx capture import [--json]
ctx capture import-provider --provider codex|claude|pi|opencode|antigravity|gemini|cursor --input <path> [--json]
ctx capture import-codex-history --input <path> [--json]
ctx capture import-pi-session --input <path> [--json]
ctx capture import-local-providers [--json]
ctx vcs inspect [path] [--json]
ctx pr parse <pull-request-url> [--json]
ctx link-pr <record-id> <pull-request-url> [--json]
ctx publish pr-comment <record-id> --dry-run [--json]
ctx export [--output ctx-records.json]
ctx import [--input ctx-records.json] [--overwrite]
ctx validate [--json]
ctx doctor [--privacy]
ctx repair [--json]
```

See [docs/cli-reference.md](docs/cli-reference.md) for the detailed current
command reference.

Small local dogfood workflows live in [examples/](examples/):

- [examples/local-record-workflow.sh](examples/local-record-workflow.sh) creates
  a temporary data root, records work, captures command evidence, searches,
  renders context, exports, and validates storage.
- [examples/capture-spool-fixture.sh](examples/capture-spool-fixture.sh) writes
  a fixture capture envelope to the local spool and imports it.

The examples default to temporary data roots under `target/tmp`. Set
`CTX_BIN` to use an already-built `ctx` binary, `CTX_EXAMPLE_DATA_ROOT` to reuse
a specific example data root, or `CTX_EXAMPLE_TMPDIR` to move temporary roots.

## Storage

By default, ctx uses machine-local storage under:

```text
~/.ctx/
  work.sqlite
  objects/
  spool/
  shims/
  config.toml
  logs/
```

Set `CTX_DATA_ROOT` or `--data-root` to use a different ctx root; ctx does not
append an extra product directory. The implementation stores records,
provider fixture summaries and rich capture rows, imported command evidence,
VCS/PR metadata, and report/search projections in SQLite. Full evidence payloads
are stored in local object files, and pending capture envelopes from fixtures or
ctx-owned shims live in the durable spool. Broad provider-history directory
scanners and native provider hooks remain explicit follow-on work; the shipped
provider-history paths are the explicit Codex and Pi import commands documented
above.

No account is required. No hosted sync runs in this branch. Exported JSON files
should be reviewed before they leave your machine because records and command
output can contain source code, prompts, paths, secrets, or customer data.

For the launch security boundary, see [SECURITY.md](SECURITY.md) and the
[ctx local recording threat model](docs/threat-model.md). Hosted/team Option A is not
part of this branch's launch scope.

## Product Direction

The ctx recording direction remains local-first:

- work records should be valuable without adopting a special agent runtime.
- Local recording should not require a hosted account.
- Passive capture should be conservative and should not break the wrapped tool
  if capture fails.
- Hosted sync should not upload raw command stdout/stderr evidence by default;
  full transcript sync, if implemented later, should be explicit opt-in.
- Pull request publishing in this branch is local GitHub PR comment upsert via
  the authenticated `gh` CLI; hosted/team publishing remains out of scope.
- PR publishing should upsert a separate ctx comment by default instead of
  mutating the PR description.
- Inferred links between records, repos, commits, and PRs should be confidence
  labeled rather than presented as facts.

These are product constraints for upcoming work, not claims that all of the
behavior exists today.

Dependency and license audit decisions for the source-build launch branch are
tracked in
[docs/dependency-license-audit.md](docs/dependency-license-audit.md). Public
installer or updater documentation should not be added until that gate is
complete.

## Build From Source

Prerequisites:

- Rust stable
- a normal local C/C++ build toolchain for your platform

Build and test:

```bash
cargo build --workspace
cargo test --workspace --all-targets
```

Run the repository check script:

```bash
./scripts/check.sh
```

If Bazel is installed:

```bash
./scripts/bazel-test.sh
bazel test //...
```
