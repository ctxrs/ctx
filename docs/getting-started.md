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
provider hooks/shims; create records explicitly and run important commands
through `ctx evidence run` when you want durable evidence.

You can also pipe a longer note into a record:

```bash
cat notes.md | ctx record --title "checkout retry notes" --body - --kind note
```

## Capture command evidence

Run commands through ctx when their output should become evidence:

```bash
ctx evidence run --record <record-id> cargo test -p checkout
```

The command is executed normally. ctx stores the command string, exit code, stdout, stderr, start time, and duration.

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
```

## Export, import, and validate

```bash
ctx export --output work-records.json
ctx import --input work-records.json
ctx validate
```

`ctx import` imports ctx JSON archives. It is not a provider-history
importer for existing local Codex, Claude, Cursor, or other agent sessions.

## Remove local product data

```bash
ctx uninstall --yes
```

Only run uninstall when you intend to remove the local Work Recorder data store.
