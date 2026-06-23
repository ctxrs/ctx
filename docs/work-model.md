# Work Model

ctx is organized around work records. A record is the durable history for one coding-agent task.

## Record

A work record has an id, title, body, kind, tags, optional workspace path, optional pull request URL, and timestamps. It should be small enough to review as one unit and complete enough that another engineer can understand the work without reading terminal scrollback.

Typical record kinds:

- `task`: a unit of coding-agent work
- `note`: a durable observation, prompt, or handoff note
- `decision`: a choice made during the work
- `review`: context for a pull request or review pass

`ctx record` creates records. `ctx list`, `ctx show`, and `ctx search` retrieve them.

## Evidence

Evidence is command output captured by `ctx evidence run` or imported from a
local Git/jj/gh shim capture. Each evidence item stores:

- the command string
- exit code
- safe stdout and stderr previews in SQLite
- full stdout and stderr as local-only object artifacts
- start time
- duration
- optional record id

This is the current local evidence model. Store file paths, reproduction notes, or links in the record body when they matter.

## Local shims

`ctx shim install --dir <path>` creates local wrapper scripts for `git`, `jj`,
and `gh`. The wrappers run the real command found later on `PATH`, preserve its
exit code, and spool command metadata plus stdout/stderr to the JSONL capture
spool. `ctx capture import` imports those pending envelopes into the local
record store. `ctx shim uninstall --dir <path>` removes only ctx-marked wrapper
scripts.

## Capture spool

The capture spool is a local JSONL queue for integrations that already know how
to emit ctx capture envelopes. `ctx capture import` turns pending envelopes into
records and evidence in the local store. Normal ctx work records commands also
import pending envelopes before serving results, so wrapper captures become
visible without a daemon.

The importer is intentionally narrower than passive history import:

- it imports files already written to the ctx spool;
- it uses stable ids derived from envelope dedupe keys when ids are omitted;
- it moves successful files to `.done`;
- it moves failed files to `.failed` and writes an error sidecar.

`ctx doctor` reports failed or stuck spool files. `ctx repair` retries failed
spool files after inspection.

Local Git/jj/gh wrapper shims are the first implemented capture writer for this
spool. Provider-native hooks remain future product direction. Provider imports
are separate commands: normalized fixture JSONL for the supported fixture
providers, Codex prompt-history JSONL with `summary_only` fidelity, Pi session
JSONL with `imported` fidelity, and conservative local provider discovery that
imports only those supported local sources.

## Pull requests

`ctx link-pr <record-id> <url>` stores a pull request URL string on a local
record. Use `ctx pr parse <url>` first to validate and normalize supported
GitHub and GitLab pull request URLs. The link stays with the local record and
appears in redacted `show`, report, and context output as well as private
archive export.

`ctx publish pr-comment <record-id>` uses the authenticated local `gh` CLI to
create or update one marker-bounded ctx comment on the linked GitHub pull
request. `--dry-run` renders the same comment locally for review. Hosted/team
publishing and non-GitHub PR publishing are outside the current local-first
implementation.

## Context and reports

`ctx context [query]` renders matching records and evidence as work context.
`ctx report` summarizes recent recorded work in text or JSON. `ctx dashboard
export` writes a local React/Vite dashboard for visual review, using bundled
local assets only. These review surfaces redact secret-like values and local
paths by default; archive export is the private full-fidelity data path.

Use these commands before review, handoff, or resuming a paused task. They turn the local record store into a concise packet of what happened.

## Workspace

A record can include the workspace where the work happened. That path gives commands and notes their execution context.

ctx does not require a special agent runtime. You can use Codex, Claude Code, Cursor, shell scripts, GitHub CLI, or a manual editor workflow. The record is the stable layer around those tools.

`ctx vcs inspect` can add repository context outside the record itself. It
detects Git metadata, redacts remotes, reports worktree state, computes a stable
repository fingerprint, and reports jj workspace metadata when `jj` is
installed. `ctx pr parse` normalizes supported GitHub and GitLab pull request
URLs before a URL is attached with `ctx link-pr`.

## Storage lifecycle

`ctx setup` creates or updates the local ctx root, installs shims, imports known
supported provider history, and opens the dashboard when appropriate. `ctx
status` prints the database path, shim status, dashboard URL/running status,
and spool pending count. `ctx dashboard` starts or reuses the localhost
dashboard server; in headless sessions or with `--no-open`, it prints the URL
and command instead of opening a browser. `ctx uninstall` removes shims, shell
integration, and any optional service while keeping recorded data; `ctx
uninstall --delete-data` deletes the local store, objects, spool, logs, and
config after confirmation.

## Boundaries

The current open recorder focuses on explicit local records, local Git/jj/gh
shim capture, and review packets. It does not yet passively capture provider
sessions or sync hosted team data. Existing local agent history import is
limited to Codex prompt history with `summary_only` fidelity. Pull request
comment publishing is limited to local GitHub PR upsert through `gh`.

Hosted team sync, shared policy enforcement, centralized dashboards, and
organization-level analytics are separate product concerns and are not part of
the current local-first implementation.

The launch threat model is documented in [threat-model.md](threat-model.md).
Hosted Option A requires a separate threat model before it can be documented as
implemented.
