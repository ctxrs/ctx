# Getting Started

ctx creates local work records for coding-agent tasks. A record keeps the
prompt or note, command evidence, pull request links, tags, workspace context,
and reportable history for one unit of work.

This page documents the public local-first ctx `0.1.0` candidate. It
is not the ctx ADE and does not claim a hosted account, hosted sync, or passive
provider-native capture beyond the proven surfaces called out below.

## Install

Public installer URLs are not documented as live for this branch yet. Build or
install from this checkout:

```bash
cargo build -p ctx
cargo install --path crates/ctx-cli
```

Verify the CLI:

```bash
ctx status
ctx schema
```

See [release-install.md](release-install.md) for the future installer contract
and the public wording guardrails around `v0.1.0`.

## Set up the local workspace

Create the local SQLite store and ctx-owned passive capture shims:

```bash
ctx setup
ctx status
```

`ctx setup` installs Git/jj/gh wrapper shims under
`${CTX_DATA_ROOT:-~/.ctx}/shims`, creates or updates the local layout, imports
known supported provider history, and opens the dashboard when an interactive
desktop/browser session is available. When shell startup files can be updated
safely, setup adds a marker-bounded block for future shells.

```bash
ctx setup --no-open
ctx setup --no-import
ctx setup --no-shell-update
ctx setup --service
ctx setup --dry-run
```

Use `--no-open` for headless sessions, SSH, CI, or any run where setup should
print the dashboard URL and command instead of opening a browser. `--service`
opts into the optional background service; no launchd/systemd/Windows service
is installed by default.

## Create a work record

Start in the repository where the work is happening.

```bash
cd ~/code/my-project
ctx record \
  --title "fix checkout retry handling" \
  --body "Investigate flaky checkout retries and make the behavior deterministic." \
  --tag checkout \
  --tag retry \
  --kind task \
  --json
```

The JSON output includes the record id. Use that id when attaching evidence or a pull request.

Run your normal agent or tools from the same workspace. ctx is designed to work beside existing CLIs instead of replacing them.

This branch passively captures supported local Git/jj/gh commands after the
shim directory is active on `PATH`. It can import normalized provider fixtures,
Codex prompt history, and explicit Pi session JSONL, but routine agent
transcript capture still needs provider-native hooks that are not implemented
here.

You can also pipe a longer note into a record:

```bash
cat notes.md | ctx record --title "checkout retry notes" --body - --kind note
```

## Capture command evidence

Run commands through ctx when their output should become evidence:

```bash
ctx evidence run --record <record-id> cargo test -p checkout
```

The command is executed normally. ctx stores the command string, exit code,
safe stdout/stderr previews, start time, and duration in SQLite, with full
stdout/stderr saved as local-only object artifacts.

## Capture local Git/jj/gh commands

Install reversible wrappers into a directory you control, or use the default
wrappers created by `ctx setup`:

```bash
ctx shim install --dir .ctx-shims
eval "$(ctx shim env --dir .ctx-shims)"
```

Commands such as `git status`, `jj log`, and `gh pr view` run through the real
tool found later on `PATH`, then best-effort spool command metadata into the
local capture spool. Import pending shim captures when you want them in the
record store:

```bash
ctx capture import
```

Remove the wrappers with:

```bash
ctx shim uninstall --dir .ctx-shims
```

`ctx status` reports whether each shim is installed and active on `PATH`, plus
pending, processing, done, and failed spool counts. Use `ctx status --json` for
stable local diagnostic JSON; the JSON form intentionally includes local
filesystem paths.

## Import provider history when it is actually proven

The current candidate keeps provider claims narrow:

- Codex has a `supported-import` path through explicit prompt-history import.
- Claude Code is `fixture-only`.
- Pi is `fixture-only`.

Useful commands:

```bash
ctx capture import-local-providers --json
ctx capture import-codex-history --input ~/.codex/history.jsonl --json
ctx capture import-provider --provider codex --input tests/fixtures/provider/codex.jsonl --json
```

The local-provider scan imports Codex history when the file exists. It reports
Claude Code and Pi directories as unsupported instead of inventing native
transcript support. See [provider-support.md](provider-support.md) for the
taxonomy and current matrix.

## Link review state

```bash
ctx pr parse https://github.com/example/project/pull/42 --json
ctx link-pr <record-id> https://github.com/example/project/pull/42
```

`ctx link-pr` stores the pull request URL string in the local record. Use
`ctx pr parse` first to validate and normalize supported GitHub and GitLab pull
request URLs.

Publish a PR comment only after previewing it locally:

```bash
ctx publish pr-comment <record-id> --dry-run
ctx publish pr-comment <record-id>
```

Publishing is local CLI-driven GitHub PR comment upsert through `gh`. It is not
hosted sync.

## Review and search

```bash
ctx list
ctx show <record-id>
ctx search checkout
ctx context checkout
ctx report
ctx dashboard
ctx dashboard export --output ./ctx-dashboard
```

`ctx context --json` and `ctx search --json` return structured packets with
match reasons, citations, result summaries, and stable record ids. If
`CTX_DASHBOARD_URL` is set to a share-safe `http://` or `https://` URL, those
JSON packets may include dashboard links. `ctx dashboard export` writes a local
React/Vite dashboard with bundled local assets and no hosted sync, tracking, or
remote assets. Default review output from `ctx list`, `ctx show`, `ctx search`,
and `ctx report` redacts secret-like values, credential URLs, and local paths.
`ctx dashboard` starts or reuses a localhost server and live-updates while it is
open as new work is recorded. In headless sessions or with `--no-open`, it
prints the URL and command instead of opening a browser.

## Inspect repository and pull request metadata

Use VCS inspection when a record body or review note needs repository context:

```bash
ctx vcs inspect --json
```

The command detects Git metadata, redacts remote URLs, reports worktree state,
and includes a stable repository fingerprint. If `jj` is installed, it also
reports the jj workspace root.

Parse a supported pull request URL before linking it:

```bash
ctx pr parse https://github.com/example/project/pull/42 --json
ctx link-pr <record-id> https://github.com/example/project/pull/42
```

## Export, import, and validate

```bash
ctx export --output ctx-records.json
ctx import --input ctx-records.json
ctx validate
```

`ctx import` imports ctx JSON archives, including evidence output payloads
exported by `ctx export`. Provider inputs use `ctx capture import-provider` or
the explicit Codex/Pi provider-history commands below.

## Import local capture spool files

The capture importer reads pending JSONL capture envelope files from:

```text
${CTX_DATA_ROOT:-~/.ctx}/spool/
```

Run:

```bash
ctx capture import --json
ctx doctor
ctx repair
```

Successful files move to `.done`; failed files move to `.failed` with an
`.error.json` sidecar. Normal ctx work records commands import pending capture
files before serving results. `ctx status` reports pending, temporary,
processing, done, and failed spool counts. `ctx validate --json` returns a
stable `valid` boolean, findings list, and spool counts for automation.
`ctx doctor` reports failed or stuck capture spool files. `ctx repair` retries
failed files.

This spool path is local integration plumbing, not a provider-history importer.
`ctx setup` installs local Git/jj/gh wrapper shims under the data root; they
write the spool only after the shim directory is active on `PATH`. Codex,
Claude, Cursor, and Pi provider-native hooks are not installed.

Provider fixture imports fail closed on malformed JSONL or provider mismatches
during CLI preflight, before any provider summary record is created. Rows that
pass CLI preflight but fail during the lower typed capture import are reported
in the failed count and can leave the provisional local summary work record
that links the attempted import.

To import local provider history explicitly:

```bash
ctx capture import-codex-history --input ~/.codex/history.jsonl --json
ctx capture import-pi-session --input ~/.pi/agent/sessions/<session>.jsonl --json
```

The Codex path is `summary_only`: it imports prompt rows grouped by Codex
`session_id`, not assistant replies, tool calls, command output, artifacts, or
child sessions. The Pi path is `imported`: it preserves message entry ids and
parent ids in metadata, but does not create ctx subagent edges or artifacts for
raw image blocks. To discover known local provider locations and import only the
safe supported sources:

```bash
ctx capture import-local-providers --json
```

That command imports Codex prompt history when `~/.codex/history.jsonl` exists
and bounded Pi session JSONL files under `~/.pi/agent/sessions` when present.
It reports Claude, OpenCode, Antigravity, Gemini, and Cursor surfaces as
fixture-only blockers when discovered; it does not invent native transcript
support. See [provider-support.md](provider-support.md).

See [../examples/local-record-workflow.sh](../examples/local-record-workflow.sh)
and [../examples/capture-spool-fixture.sh](../examples/capture-spool-fixture.sh)
for small local dogfood flows.

For privacy defaults and local triage, continue with:

- [privacy-storage.md](privacy-storage.md)
- [troubleshooting.md](troubleshooting.md)
- [hosted-sync-roadmap.md](hosted-sync-roadmap.md)

## Remove local product data

```bash
ctx uninstall --yes
```

`ctx uninstall` removes shims, the managed shell rc block, and the optional
service if it was installed. It keeps recorded data by default. Delete local
records, object payloads, spool files, logs, and config only with an explicit
data deletion request:

```bash
ctx uninstall --delete-data --yes
```
