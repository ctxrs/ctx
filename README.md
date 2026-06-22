<p align="center">
  <img src="assets/readme/work-record-banner.png" alt="ctx Work Recorder" />
</p>

ctx is being productized around **Work Records**: durable, local records of
agent-assisted work that can be searched, reviewed, exported, and later attached
to pull requests or team workflows.

This branch is an early local Work Recorder. It is useful today for explicit
records, command evidence, pull request links, search, reports, context output,
JSON export/import, and local storage validation. It is not yet the full passive
recorder described in the product direction.

## Current Status

Implemented in this branch:

- create local Work Records with title, body, tags, kind, optional workspace,
  timestamps, and id;
- capture command evidence when commands are run through `ctx evidence run`;
- link one pull request URL to a record with `ctx link-pr`;
- list, show, search, and render context for local records;
- generate text or JSON reports from recent records and evidence;
- export/import ctx JSON archives;
- validate and remove the local Work Recorder data store.

Not implemented yet:

- a local dashboard;
- passive provider hooks, shell hooks, or Git/jj/gh shims;
- importing existing Codex, Claude, Cursor, or other local agent history;
- posting or updating pull request comments;
- hosted sync, hosted sharing, accounts, or team policy;
- public installer URLs for this branch;
- dashboard and publish commands such as `ctx dashboard` or `ctx publish`.

The implemented CLI now uses root-level Work Recorder commands. The older
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
cargo run -p ctx -- workspace status
cargo run -p ctx -- list
```

## Quick Start

Create the local Work Recorder store:

```bash
ctx setup
ctx status
```

Create a Work Record:

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

Link a pull request URL locally:

```bash
ctx link-pr <record-id> https://github.com/example/project/pull/42
```

Review and search:

```bash
ctx list
ctx show <record-id>
ctx search checkout
ctx context checkout
ctx report
```

Move records between machines with ctx JSON archives:

```bash
ctx export --output work-records.json
ctx import --input work-records.json
```

`ctx import` imports ctx archive JSON only. It does not import existing
local agent history from provider transcript directories.

## Work Record Model

A Work Record is the durable history for one unit of agent-assisted work. The
current implementation stores:

- id;
- title;
- body;
- kind;
- tags;
- optional workspace path;
- optional pull request URL;
- created and updated timestamps;
- command evidence captured by `ctx evidence run`.

The near-term product direction is broader: Work Records should eventually
connect sessions, subagents, command evidence, tool output, files touched,
commits, pull requests, artifacts, summaries, decisions, and review notes. Those
larger objects are direction unless the CLI reference documents a shipped
command for them.

## CLI

The current command groups are:

```bash
ctx setup
ctx status
ctx uninstall --yes

ctx schema
ctx record --title "task title" --body "prompt or note" --kind task
ctx list
ctx show <record-id>
ctx search <query>
ctx context [query]
ctx report
ctx evidence run [--record <record-id>] <command> [args...]
ctx link-pr <record-id> <pull-request-url>
ctx export [--output work-records.json]
ctx import [--input work-records.json] [--overwrite]
ctx validate
```

See [docs/cli-reference.md](docs/cli-reference.md) for the detailed current
command reference.

## Storage

By default, ctx uses machine-local storage under:

```text
~/.ctx/work-record/
  work.sqlite
```

Set `CTX_DATA_ROOT` to use a different root. The current implementation stores
records and command evidence in SQLite. Blob storage, JSONL capture inboxes, and
passive normalization pipelines are planned Work Recorder architecture, not
current branch behavior.

No account is required. No hosted sync runs in this branch. Exported JSON files
should be reviewed before they leave your machine because records and command
output can contain source code, prompts, paths, secrets, or customer data.

## Product Direction

The Work Recorder direction remains local-first:

- Work Records should be valuable without adopting a special agent runtime.
- Local recording should not require a hosted account.
- Passive capture should be conservative and should not break the wrapped tool
  if capture fails.
- Hosted sync should not upload raw transcripts by default; full transcript sync
  should be explicit opt-in.
- Pull request publishing should eventually upsert a separate ctx comment by
  default instead of mutating the PR description.
- Inferred links between records, repos, commits, and PRs should be confidence
  labeled rather than presented as facts.

These are product constraints for upcoming work, not claims that all of the
behavior exists today.

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
