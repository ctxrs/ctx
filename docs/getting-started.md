# Getting Started

ctx creates local Work Records for coding-agent tasks. A record keeps the prompt or note, command evidence, pull request links, tags, workspace context, and reportable history for one unit of work.

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

## Set up the local workspace

Create the local SQLite store and ctx-owned passive capture shims:

```bash
ctx setup
ctx status
```

`ctx setup` installs Git/jj/gh wrapper shims under
`${CTX_DATA_ROOT:-~/.ctx}/work-record/shims` and prints the `PATH` export needed
to activate them. To have ctx add a marker-bounded block to a shell rc file:

```bash
ctx setup --shell-rc ~/.zshrc
```

The shell rc change is backed up and can be removed with
`ctx shim deactivate-shell --dir ~/.ctx/work-record/shims --shell-rc ~/.zshrc`.

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
shim directory is active on `PATH`. It can import normalized provider fixtures
and Codex prompt history, but routine agent transcript capture still needs
provider-native hooks that are not implemented here.

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
stdout/stderr saved as local-only blob artifacts.

## Capture local Git/jj/gh commands

Install reversible wrappers into a directory you control, or use the default
wrappers created by `ctx setup`:

```bash
ctx shim install --dir .ctx-shims
eval "$(ctx shim env --dir .ctx-shims)"
```

Commands such as `git status`, `jj log`, and `gh pr view` run through the real
tool found later on `PATH`, then best-effort spool command metadata into the
local capture inbox. Import pending shim captures when you want them in the
record store:

```bash
ctx capture import
```

Remove the wrappers with:

```bash
ctx shim uninstall --dir .ctx-shims
```

`ctx status` reports whether each shim is installed and active on `PATH`, plus
pending, processing, done, and failed spool counts.

## Link review state

```bash
ctx pr parse https://github.com/example/project/pull/42 --json
ctx link-pr <record-id> https://github.com/example/project/pull/42
```

`ctx link-pr` stores the pull request URL string in the local record. Use
`ctx pr parse` first to validate and normalize supported GitHub and GitLab pull
request URLs.

## Review and search

```bash
ctx list
ctx show <record-id>
ctx search checkout
ctx context checkout
ctx report
ctx dashboard export --output ./work-record-dashboard
```

`ctx context --json` and `ctx search --json` return structured packets with
match reasons, citations, result summaries, and stable record ids. If
`CTX_DASHBOARD_URL` is set to a share-safe `http://` or `https://` URL, those
JSON packets may include dashboard links. `ctx dashboard export` writes a local
React/Vite dashboard with bundled local assets and no hosted sync, tracking, or
remote assets. Default review output from `ctx list`, `ctx show`, `ctx search`,
and `ctx report` redacts secret-like values, credential URLs, and local paths.

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
ctx export --output work-records.json
ctx import --input work-records.json
ctx validate
```

`ctx import` imports ctx JSON archives, including evidence output payloads
exported by `ctx export`. Provider inputs use `ctx capture import-provider` or
the explicit Codex prompt-history command below.

## Import local capture spool files

The capture importer reads pending JSONL capture envelope files from:

```text
${CTX_DATA_ROOT:-~/.ctx}/work-record/inbox/
```

Run:

```bash
ctx capture import --json
ctx doctor
ctx repair
```

Successful files move to `.done`; failed files move to `.failed` with an
`.error.json` sidecar. Normal Work Recorder commands import pending capture
files before serving results. `ctx status` reports pending, temporary,
processing, done, and failed spool counts. `ctx doctor` reports failed or stuck
capture spool files. `ctx repair` retries failed files.

This spool path is local integration plumbing, not a provider-history importer.
`ctx setup` installs local Git/jj/gh wrapper shims under the data root; they
write the spool only after the shim directory is active on `PATH`. Codex,
Claude, Cursor, and Pi provider-native hooks are not installed.

Provider fixture imports fail closed on malformed JSONL or provider mismatches
during CLI preflight, before any provider summary record is created. Rows that
pass CLI preflight but fail during the lower typed capture import are reported
in the failed count and can leave the provisional local summary Work Record
that links the attempted import.

To import a local Codex prompt history file explicitly:

```bash
ctx capture import-codex-history --input ~/.codex/history.jsonl --json
```

This path is `summary_only`: it imports prompt rows grouped by Codex
`session_id`, not assistant replies, tool calls, command output, artifacts, or
child sessions. To discover known local provider locations and import only the
safe supported sources:

```bash
ctx capture import-local-providers --json
```

That command imports Codex prompt history when `~/.codex/history.jsonl` exists.
It reports Claude and Pi local directories as unsupported when discovered; it
does not invent native transcript support. See [provider-support.md](provider-support.md).

See [../examples/local-record-workflow.sh](../examples/local-record-workflow.sh)
and [../examples/capture-spool-fixture.sh](../examples/capture-spool-fixture.sh)
for small local dogfood flows.

## Remove local product data

```bash
ctx uninstall --yes
```

Only run uninstall when you intend to remove the local Work Recorder data store.
If setup wrote a shell rc activation block, pass the same file to remove it:

```bash
ctx uninstall --yes --shell-rc ~/.zshrc
```
