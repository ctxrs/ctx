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

Create the local SQLite store:

```bash
ctx setup
ctx status
```

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

This branch does not yet passively import existing agent history or install
provider hooks; create records explicitly and run important commands through
`ctx evidence run` when you want durable evidence.

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

Install reversible wrappers into a directory you control:

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

## Link review state

```bash
ctx link-pr <record-id> https://github.com/example/project/pull/42
```

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
match reasons, citations, and attached evidence. If `CTX_DASHBOARD_URL` is set
to a share-safe `http://` or `https://` URL, those JSON packets may include
dashboard links. `ctx dashboard export` writes a static local HTML dashboard
with no hosted sync, JavaScript, tracking, or remote assets.

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
exported by `ctx export`. It is not a provider-history importer for existing
local Codex, Claude, Cursor, or other agent sessions.

## Import local capture spool files

The capture importer reads pending JSONL capture envelope files from:

```text
${CTX_DATA_ROOT:-~/.ctx}/work-record/inbox/
```

Run:

```bash
ctx capture import --json
```

Successful files move to `.done`; failed files move to `.failed` with an
`.error.json` sidecar. `ctx status` reports pending, temporary, processing,
done, and failed spool counts. `ctx validate` reports failed or stuck capture
spool files.

This is local integration plumbing, not a provider-history importer. The branch
includes opt-in local Git/jj/gh wrapper shims, but does not install Codex,
Claude, Cursor provider hooks or shell hooks that write the spool
automatically.

See [../examples/local-record-workflow.sh](../examples/local-record-workflow.sh)
and [../examples/capture-spool-fixture.sh](../examples/capture-spool-fixture.sh)
for small local dogfood flows.

## Remove local product data

```bash
ctx uninstall --yes
```

Only run uninstall when you intend to remove the local Work Recorder data store.
