<p align="center">
  <img src="assets/readme/work-record-banner.png" alt="ctx records agent work so it can be attached to PRs, searched later, and shared across teams." />
</p>

Agent work disappears too easily. Prompts, decisions, failed attempts, tool output, review notes, and implementation context get scattered across local session files, terminal history, and provider UIs.

ctx gives that work a durable local record. It captures prompts, transcripts, commands, tool calls, evidence, artifacts, commits, PR links, and summaries in a local database that humans and future agents can search.

The fastest way to see it is to run setup. ctx imports the agent history already on your machine, links what it can to repos and PRs, and opens a local dashboard.

## Why Use ctx

- **Attach agent history to PRs.** Link a pull request to the record that produced it: prompt, commands, evidence, artifacts, and decisions.
- **Search previous attempts.** Future agents can find old context instead of starting from zero after compaction or a fresh session.
- **Preserve subagent work.** Manager sessions, implementation subagents, review subagents, and imported provider transcripts stay connected.
- **Use your existing agents and VCS.** ctx works beside Claude Code, Codex, Pi, Cursor, shell scripts, Git, jj, GitHub CLI, and local editors.
- **Keep records local first.** Your local records live on your machine in SQLite and blob files. Nothing has to be pushed or synced by default.
- **Handle multi-repo work.** A single record can touch multiple Git repos, commits, files, and PRs.

## Install

macOS, Linux, and FreeBSD:

```bash
curl -fsSL https://ctx.rs/install | sh
```

Windows PowerShell:

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://ctx.rs/install.ps1 | iex"
```

Set up ctx and open the local dashboard:

```bash
ctx setup
```

`ctx setup` creates local storage, imports existing agent history, configures optional capture integrations, starts the local dashboard, and opens it in your browser. It does not write files into your Git repo by default.

Use `--no-open` for headless setup:

```bash
ctx setup --no-open
```

Check the install:

```bash
ctx doctor
```

## Quick Start

Run setup first:

```bash
ctx setup
```

The dashboard opens with imported history and updates as new work is recorded. Keep using your agents and tools normally.

Search past work:

```bash
ctx search "checkout retry"
```

Give a future agent context:

```bash
ctx context "checkout retry"
```

Open the dashboard again:

```bash
ctx dashboard
```

Generate a review report:

```bash
ctx report <record-id> --format markdown
```

Publish a PR report:

```bash
ctx publish pr <record-id>
```

Manual annotation is optional. Use it when passive capture cannot infer something important:

```bash
ctx attach pr <record-id> https://github.com/acme/app/pull/123
ctx attach note <record-id> "The first attempt failed because the fixture was stale."
```

## The Core Model

ctx is organized around **Work Records**.

A Work Record is the durable history for one piece of agent-assisted work. It can include:

- the original prompt or task brief;
- user and agent messages;
- primary agent and subagent sessions;
- command evidence;
- tool calls and tool output;
- files touched;
- commits and PR links;
- screenshots and artifacts;
- summaries, decisions, and review notes.

The main objects are:

| Object | Meaning |
| --- | --- |
| `Work Record` | The task or unit of work you want to preserve. |
| `Session` | One agent conversation, including primary sessions and subagents. |
| `Run` | One bounded execution: an agent turn, command, review pass, import, or evidence capture. |
| `Event` | A normalized timeline item: message, tool call, command output, file change, etc. |
| `Evidence` | A reviewable result such as a test run, lint command, screenshot, or CI link. |
| `Artifact` | A large payload such as stdout, transcript, screenshot, report, or diff. |
| `Repo` | A detected Git or jj repository associated with a record/session/event. |
| `PR` | A pull request attached to a record. |

## CLI

Most users should not need to manually record normal work. The main commands are for setup, dashboard access, search, reports, sharing, and occasional repair/annotation.

The main commands are:

```bash
ctx setup
ctx status
ctx doctor
ctx record
ctx run
ctx attach
ctx publish
ctx dashboard
ctx show
ctx list
ctx search
ctx context
ctx report
ctx export
ctx import
ctx uninstall
```

### `ctx setup`

Creates local ctx storage, imports existing agent history, configures optional capture integrations, starts the local dashboard, and opens it in your browser.

```bash
ctx setup
ctx setup --with-hooks
ctx setup --with-shims
ctx setup --no-open
```

By default, setup is local and conservative. Repo-level Git hooks are optional. The default policy is no repo files unless you ask for them.

Setup should be the quick local demo:

1. detect existing agent histories;
2. import supported provider sessions;
3. infer repos from cwd, transcript paths, and Git roots;
4. link what it can to commits and PRs;
5. start the dashboard;
6. open the dashboard in your browser.

### `ctx dashboard`

Opens the local dashboard.

```bash
ctx dashboard
```

The dashboard is the always-on view of recorded agent work on the machine. It refreshes as new records, sessions, tool calls, commands, PR links, and artifacts are captured.

Common views:

- overview of recent work;
- active sessions;
- repo view;
- record/task view;
- session transcript view;
- PR evidence view;
- slow/flaky command view;
- missing-evidence and stale-evidence view.

### `ctx status`

Shows the current recorder state.

```bash
ctx status
```

Useful fields include:

- data directory;
- SQLite path;
- detected workspace;
- detected Git repos;
- installed hooks/shims;
- recent capture health.

### `ctx doctor`

Checks whether ctx can record successfully.

```bash
ctx doctor
```

It verifies storage, permissions, optional shims, optional hooks, Git detection, and provider capture configuration.

### `ctx record`

Creates a Work Record.

```bash
ctx record "Refactor auth refresh" \
  --body "Move token refresh into a shared provider module." \
  --tag auth --tag refactor
```

Most records should be created by passive capture, imports, hooks, or inferred grouping. Humans and agents can call this when they need an explicit record that ctx cannot infer. A useful agent instruction is:

```text
When you begin meaningful work, create or reuse a ctx record. Attach important commands, decisions, and PRs to that record.
```

### `ctx run`

Runs a command and stores the output as evidence.

```bash
ctx run --record <record-id> -- npm test
ctx run --record <record-id> -- cargo test -p ctx-cli
ctx run --record <record-id> -- pnpm lint
```

ctx stores:

- command;
- cwd;
- start/end time;
- exit code;
- stdout/stderr preview;
- full output blob when large;
- timeout/truncation status.

### `ctx attach`

Attaches external context to a record.

```bash
ctx attach pr <record-id> https://github.com/acme/app/pull/123
ctx attach commit <record-id> abc123
ctx attach artifact <record-id> screenshot.png
ctx attach note <record-id> "The first attempt failed because the fixture was stale."
```

### `ctx publish`

Publishes a shareable report.

```bash
ctx publish pr <record-id>
ctx publish pr <record-id> --summary-only
ctx publish pr <record-id> --with-evidence
```

For a PR, ctx should post or update a separate Markdown comment with a stable marker rather than editing the PR description by default.

Example:

```md
<!-- ctx:work-record record_id=... -->
## ctx Work Record

- record: ...
- sessions: ...
- commands/tests: ...
- evidence: ...
- artifacts: ...
- known gaps: ...
```

The local full record stays private unless you explicitly export, bundle, or sync it.

### `ctx show`

Shows one record.

```bash
ctx show <record-id>
ctx show <record-id> --json
```

### `ctx list`

Lists recent records.

```bash
ctx list
ctx list --repo acme/app
ctx list --tag auth
```

### `ctx search`

Searches records, messages, commands, evidence, and summaries.

```bash
ctx search "token refresh"
ctx search "checkout retry" --repo app
ctx search "race condition" --primary
ctx search "race condition" --subagents
```

Search defaults should prefer high-signal material:

1. explicit record titles and notes;
2. user messages in primary sessions;
3. manager summaries and decisions;
4. review conclusions;
5. subagent final summaries;
6. subagent internal messages;
7. raw tool output.

### `ctx context`

Prints agent-readable context for a future session.

```bash
ctx context "checkout retry"
ctx context --repo app "auth refresh"
ctx context --record <record-id>
```

This is one of the most important commands. It turns recorded history into compact context that a future agent can use.

### `ctx report`

Creates a human-readable report.

```bash
ctx report <record-id>
ctx report <record-id> --format markdown
ctx report <record-id> --format html
ctx report <record-id> --format json
```

A report can include:

- what changed;
- why it changed;
- linked PRs/commits;
- commands/tests run;
- stale or missing evidence;
- artifacts and screenshots;
- transcript links;
- summaries and review notes.

### `ctx export` and `ctx import`

Moves records between machines or archives them.

```bash
ctx export --since 7d > ctx-records.json
ctx import ctx-records.json
```

Use export/import when moving to a new machine, pulling records back from a remote devbox, or preserving work from an ephemeral agent job.

### `ctx uninstall`

Removes local ctx setup.

```bash
ctx uninstall
ctx uninstall --data
```

`ctx uninstall` removes shims/hooks/configuration. `ctx uninstall --data` also removes local Work Record data after confirmation.

## Capture Methods

ctx collects work through several mechanisms.

### Direct CLI Capture

The most reliable path is explicit:

```bash
ctx record
ctx run
ctx attach
```

This works for humans, shell scripts, and agents.

### Agent-Mediated Capture

Agents can call ctx directly. This is flexible and works even when provider hooks are limited.

Example instruction:

```text
Use ctx to preserve important work. Create a record for the task, attach important test commands with ctx run, attach the PR when created, and use ctx context before resuming old work.
```

### Provider Hooks

Provider hooks capture agent events when available.

Claude Code and Codex expose hook events for sessions, prompts, tools, subagents, and compaction-related lifecycle events. ctx records these events and imports provider transcript files when available.

Capture fidelity depends on the provider. Every imported session/event is labeled with its source and fidelity:

```text
full
partial
imported
inferred
summary_only
```

### Subagent Capture

Subagents are stored as normal sessions with parent-child relationships.

```text
primary session
  -> implementation subagent
  -> review subagent
  -> research subagent
```

Roles are not hard-coded. A subagent may review, implement, investigate, or do all three. ctx stores provider labels and optional role hints, but the graph remains flexible.

### Git, jj, and GitHub Capture

ctx can capture Git, jj, and GitHub CLI activity through optional shims or hooks.

Useful captured facts include:

- current repo/workspace;
- branch, bookmark, or change context;
- commits and jj changes;
- changed files;
- PR URL/number;
- PR creation/update events;
- command cwd;
- command timestamp.

The shims must always pass through to the real command. If ctx capture fails, `git`, `jj`, and `gh` should still work.

### Provider Imports

ctx can import existing provider history from local session stores, such as Codex and Claude transcript directories.

Imported records are useful, but they are marked as imported rather than first-party captured.

## Multi-Repo Work

ctx uses one local machine-level database.

```text
~/.ctx/work-record/work.sqlite
```

A Work Record can touch many repos and VCS workspaces.

Example:

```text
record: "ship hosted work report"
  repo: ctx
  repo: ctx-private
  repo: control-plane
  pr: ctxrs/ctx#123
  pr: ctxrs/ctx-private#456
```

ctx associates sessions and events with repos/workspaces by looking at:

- session cwd;
- Git and jj root detection;
- file paths;
- `git` commands;
- `jj` commands;
- `gh` commands;
- commits and jj changes;
- PR URLs;
- explicit agent/user attachments.

Associations include a reason and confidence level, so inferred links are not treated the same as explicit links.

## Multi-Machine Work

Local-first means each runtime can record locally.

```text
MacBook          ~/.ctx/work-record/work.sqlite
remote devbox   ~/.ctx/work-record/work.sqlite
cloud job       temporary ctx archive or sync upload
```

For local-only use, export/import is enough:

```bash
ctx export --since 7d > records.json
scp devbox:records.json .
ctx import records.json
```

For teams or cloud agents, hosted sync can collect records from many machines. That is a separate layer on top of the local recorder. Local recording does not require an account.

## Storage

ctx stores local data under `~/.ctx` on the machine:

```text
~/.ctx/work-record/
  work.sqlite
  blobs/
  inbox/
```

No repo files are written by default. A MacBook, remote devbox, CI runner, or cloud agent runtime each has its own local `~/.ctx` unless you explicitly export, import, or sync records.

### SQLite

SQLite is the canonical local store. It tracks records, sessions, runs, events, evidence, repos/VCS workspaces, PRs, files, summaries, and search indexes.

### Blobs

Large payloads are stored outside SQLite by content hash:

```text
~/.ctx/work-record/blobs/<hash>
```

Examples:

- long stdout/stderr;
- full transcripts;
- screenshots;
- reports;
- artifacts;
- diffs.

SQLite stores metadata, preview text, blob IDs, sizes, hashes, truncation status, and sensitivity labels.

### JSONL Inbox

Hooks and shims write append-only JSONL events first:

```text
~/.ctx/work-record/inbox/*.jsonl
```

The inbox is a capture buffer. ctx normalizes those events into SQLite.

This keeps capture robust: a shim can write one JSON line and pass through to the real command without blocking user work.

## Privacy

ctx is local-first.

- No account is required for local recording.
- No data is uploaded by default.
- Repo files are not modified by default.
- Large outputs can be truncated or omitted.
- Sensitive events can be excluded from sync/export.
- Export and sync paths can exclude sensitive records, raw transcripts, and large blobs.

Use:

```bash
ctx status
ctx doctor
ctx export
ctx uninstall
```

to inspect and control local state.

## For Future Agents

The Work Record is useful because agents can read it.

At the start of a task:

```bash
ctx context "billing retry bug"
```

Before reviewing a PR:

```bash
ctx report <record-id> --format markdown
```

When resuming old work:

```bash
ctx search "why did we avoid the cache rewrite"
ctx show <record-id>
```

The goal is simple: the next agent should not start from zero.

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
