# CLI Reference

The Work Recorder is CLI-first. These examples match the implemented command surface.

The primary CLI uses root-level Work Recorder commands. The older
`ctx workspace ...` and `ctx work ...` forms remain as hidden compatibility
aliases for the current local behavior.

## Workspace

```bash
ctx setup
ctx status
ctx uninstall --yes
```

- `setup` creates the local Work Recorder data store.
- `status` prints the data root, work record directory, database path, and initialization state.
- `uninstall --yes` removes local Work Recorder product data.

## Schema

```bash
ctx schema
```

Prints the local SQLite schema.

## Records

```bash
ctx record --title "task title" --body "prompt, note, or summary" --tag cli --kind task
ctx record --title "long note" --body - --kind note
ctx list
ctx list --limit 50 --json
ctx show <record-id>
ctx show <record-id> --json
ctx search checkout
ctx search checkout --limit 10 --json
```

- `record` creates a Work Record.
- `--title` is required.
- `--body` accepts inline text. Use `--body -` to read from stdin.
- `--tag` may be repeated.
- `--kind` defaults to `note`.
- `--workspace` can set an explicit workspace path.
- `list`, `show`, and `search` read records back from the local store.

## Context and reports

```bash
ctx context
ctx context checkout
ctx context checkout --limit 20 --json
ctx report
ctx report --format json
```

- `context` renders records and evidence for a query as Markdown by default.
- `report` summarizes recent records and evidence as text or JSON.

## Evidence

```bash
ctx evidence run cargo test
ctx evidence run --record <record-id> cargo test -p checkout
```

`evidence run` executes the command and stores its command string, exit code, stdout, stderr, start time, and duration. Use `--record <record-id>` to attach the evidence to a specific record.

## Pull requests

```bash
ctx link-pr <record-id> https://github.com/example/project/pull/42
ctx link-pr <record-id> https://github.com/example/project/pull/42 --json
```

Attaches a pull request URL to a Work Record in the local store. This does not
publish, create, or update a pull request comment.

## Export, import, and validate

```bash
ctx export
ctx export --output work-records.json
ctx import --input work-records.json
cat work-records.json | ctx import
ctx validate
```

- `export` writes a JSON archive to stdout or `--output`.
- `import` reads a JSON archive from `--input` or stdin.
- `import` handles ctx JSON archives only; it does not import local agent
  provider history.
- `validate` checks local Work Recorder storage and prints `valid` when no findings are found.

## Not yet implemented

This branch does not include a dashboard, hosted sync, passive provider hooks,
Git/jj/gh shims, public installer flow, or pull request comment publisher.
