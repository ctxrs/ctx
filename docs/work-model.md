# Work Model

ctx is organized around Work Records. A record is the durable history for one coding-agent task.

## Record

A Work Record has an id, title, body, kind, tags, optional workspace path, optional pull request URL, and timestamps. It should be small enough to review as one unit and complete enough that another engineer can understand the work without reading terminal scrollback.

Typical record kinds:

- `task`: a unit of coding-agent work
- `note`: a durable observation, prompt, or handoff note
- `decision`: a choice made during the work
- `review`: context for a pull request or review pass

`ctx record` creates records. `ctx list`, `ctx show`, and `ctx search` retrieve them.

## Evidence

Evidence is command output captured by `ctx evidence run`. Each evidence item stores:

- the command string
- exit code
- stdout and stderr
- start time
- duration
- optional record id

This is the current local evidence model. Store file paths, reproduction notes, or links in the record body when they matter.

## Pull requests

`ctx link-pr <record-id> <url>` attaches a pull request URL to a record. The link stays with the local record and appears in `show`, reports, exports, and context.

## Context and reports

`ctx context [query]` renders matching records and evidence as work context. `ctx report` summarizes recent recorded work in text or JSON.

Use these commands before review, handoff, or resuming a paused task. They turn the local record store into a concise packet of what happened.

## Workspace

A record can include the workspace where the work happened. That path gives commands and notes their execution context.

ctx does not require a special agent runtime. You can use Codex, Claude Code, Cursor, shell scripts, GitHub CLI, or a manual editor workflow. The record is the stable layer around those tools.

## Storage lifecycle

`ctx setup` creates the local store, `ctx status` prints its paths and initialization state, and `ctx uninstall --yes` removes the local Work Recorder product data.

## Boundaries

The current open recorder focuses on explicit local records and review packets.
It does not yet passively capture provider sessions, import existing local agent
history, install Git/jj/gh shims, publish pull request comments, or open a local
dashboard.

Hosted team sync, shared policy enforcement, centralized dashboards, and
organization-level analytics are separate product concerns and are not part of
the current local-first implementation.
