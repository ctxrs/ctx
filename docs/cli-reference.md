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
ctx dashboard export --output ./work-record-dashboard
```

- `context` renders records and evidence for a query as Markdown by default.
- `report` summarizes recent records and evidence as text or JSON.
- `dashboard export` writes a static local HTML report to `index.html` in the
  output directory. It includes summary metrics, recent records, PR links,
  evidence previews, tags, and capture/search cues. The file has no hosted
  sync, tracking, JavaScript, or remote assets; review it before sharing.

## Evidence

```bash
ctx evidence run cargo test
ctx evidence run --record <record-id> cargo test -p checkout
ctx evidence run --record <record-id> --timeout-seconds 30 --max-output-bytes 32768 cargo test -p checkout
```

`evidence run` executes the command and stores its command string, exit code,
safe stdout/stderr previews, start time, and duration in SQLite. Full
stdout/stderr content is stored as local-only blob artifacts. Use
`--record <record-id>` to attach the evidence to a specific record.

- `--max-output-bytes` caps the stored stdout and stderr payloads per stream.
- `--timeout-seconds` kills the command after the timeout and records exit code
  `124`.

If `--record` is omitted, ctx creates a small `evidence` Work Record for the
captured command.

## Local shims

```bash
ctx shim install --dir .ctx-shims
ctx shim env --dir .ctx-shims
ctx shim uninstall --dir .ctx-shims
```

- `shim install` writes local wrapper scripts for `git`, `jj`, and `gh` into
  the chosen directory.
- `shim env` prints a shell `PATH` export that places that directory before the
  real tools.
- `shim uninstall` removes only wrapper scripts marked as ctx-created shims.

The wrappers run the real command found later on `PATH`, preserve its exit code,
and best-effort spool command metadata plus stdout/stderr into the local JSONL
capture inbox. They do not install repository hooks or start a daemon.

## Capture spool

```bash
ctx capture import
ctx capture import --json
```

`capture import` imports pending JSONL capture envelope files from the local
Work Recorder inbox. The inbox path is printed by `ctx status`.

- pending files end in `.jsonl`;
- successfully imported files move to `.jsonl.done`;
- failed imports move to `.jsonl.failed` and get a `.error.json` sidecar;
- `ctx status` prints spool counts;
- `ctx validate` reports failed or still-processing spool files.

This command imports ctx capture envelopes only. It does not scan existing
agent transcript directories. Local Git/jj/gh wrapper shims are opt-in through
`ctx shim`; provider-native hooks and shell hooks are not implemented in this
branch.

## VCS and pull request helpers

```bash
ctx vcs inspect
ctx vcs inspect /path/to/repo --json
ctx pr parse https://github.com/example/project/pull/42
ctx pr parse https://gitlab.com/example/project/-/merge_requests/42 --json
```

- `vcs inspect` reports Git workspace metadata, redacted remotes, worktree
  state, a stable repository fingerprint, and jj workspace metadata when `jj`
  is installed.
- `pr parse` parses supported GitHub pull request URLs and GitLab merge request
  URLs into provider, owner, repo, number, normalized URL, and confidence.

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
ctx doctor
ctx repair
ctx repair --json
```

- `export` writes a JSON archive to stdout or `--output`, including local blob
  payloads needed to preserve evidence output.
- `import` reads a JSON archive from `--input` or stdin.
- `import` handles ctx JSON archives only; it does not import local agent
  provider history.
- `validate` checks local Work Recorder storage and prints `valid` when no findings are found.
- `doctor` runs the same local health checks using the product-facing command
  name.
- `repair` retries failed capture spool files and imports anything that succeeds.

Normal Work Recorder commands import pending capture spool files before serving
results. Failed imports are retained as `.failed` files for inspection and
retry.

## Not yet implemented

This branch does not include hosted sync, passive provider hooks beyond the
local Git/jj/gh wrapper shims, public installer flow, hosted/team Option A, or
pull request comment publisher.
