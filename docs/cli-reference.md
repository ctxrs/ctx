# CLI Reference

The Work Recorder is CLI-first. These examples match the implemented command surface.

The primary CLI uses root-level Work Recorder commands. The older
`ctx workspace ...` and `ctx work ...` forms remain as hidden compatibility
aliases for the current local behavior.

## Workspace

```bash
ctx setup
ctx setup --shell-rc ~/.zshrc
ctx status
ctx uninstall --yes
ctx uninstall --yes --shell-rc ~/.zshrc
```

- `setup` creates the local Work Recorder data store and installs ctx-owned
  Git/jj/gh passive capture shims under the data root.
- `setup --shell-rc` adds a marker-bounded `PATH` activation block to the shell
  rc file and writes a backup first.
- `status` prints the data root, work record directory, database path,
  initialization state, shim status, active-on-`PATH` count, and capture spool
  counts.
- `uninstall --yes` removes local Work Recorder product data. Add `--shell-rc`
  to remove the ctx marker-bounded activation block at the same time.

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
- Human `list`, `show`, and `search` output plus `list --json`, `show --json`,
  and `search --json` are share-safe by default: secret-like values, credential
  URLs, and local paths are redacted.

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
- `report` summarizes recent records and evidence as share-safe text or JSON.
  Summary tags and report records are redacted by default.
- `dashboard export` writes a local React/Vite dashboard to `index.html` in the
  output directory, with bundled local assets. It includes summary metrics,
  recent records, PR links, evidence previews, tags, and capture/search cues.
  The export has no hosted sync, tracking, or remote assets; review it before
  sharing.

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
ctx shim activate-shell --dir .ctx-shims --shell-rc ~/.zshrc
ctx shim deactivate-shell --dir .ctx-shims --shell-rc ~/.zshrc
ctx shim uninstall --dir .ctx-shims
```

- `shim install` writes local wrapper scripts for `git`, `jj`, and `gh` into
  the chosen directory.
- `shim env` prints a shell `PATH` export that places that directory before the
  real tools.
- `shim activate-shell` and `shim deactivate-shell` manage only the ctx
  marker-bounded shell rc block and back up the file before changing it.
- `shim uninstall` removes only wrapper scripts marked as ctx-created shims.

The wrappers run the real command found later on `PATH`, preserve its exit code,
and best-effort spool command metadata plus stdout/stderr into the local JSONL
capture inbox. They do not install repository hooks or start a daemon.

## Capture spool

```bash
ctx capture import
ctx capture import --json
ctx capture import-provider --provider codex --input tests/fixtures/provider/codex.jsonl --json
ctx capture import-provider --provider claude --input tests/fixtures/provider/claude.jsonl
ctx capture import-provider --provider pi --input tests/fixtures/provider/pi.jsonl
ctx capture import-codex-history --input ~/.codex/history.jsonl --json
ctx capture import-local-providers --json
```

`capture import` imports pending JSONL capture envelope files from the local
Work Recorder inbox. The inbox path is printed by `ctx status`.

- pending files end in `.jsonl`;
- successfully imported files move to `.jsonl.done`;
- failed imports move to `.jsonl.failed` and get a `.error.json` sidecar;
- `ctx status` prints spool counts;
- `ctx validate` reports failed or still-processing spool files.

`capture import-provider` imports normalized provider fixture JSONL for
`codex`, `claude`, or `pi`. It stores provider sessions/events through the rich
capture library path and creates a local summary Work Record when new sessions
or events are imported, so `ctx search`, `ctx context`, `ctx report`, and
`ctx dashboard export` have useful review material immediately. Re-running the
same fixture is idempotent for the summary record.

Malformed provider fixture JSONL and provider mismatches fail during CLI
preflight before any provider summary record is created. Rows that pass CLI
preflight but fail during the lower typed capture import are reported as failed
in text or JSON output; that lower-import failure can leave the provisional
local summary Work Record that links the attempted import.

`capture import-codex-history` imports a Codex prompt-history JSONL file only
when you provide the input path explicitly. The observed Codex history format
contains `session_id`, unix `ts`, and prompt `text`; imported rows are marked
with `source_format=codex_history_jsonl` and `fidelity=summary_only`. This path
does not capture assistant replies, tool calls, command output, artifacts, or
child session relationships.

`capture import-local-providers` checks known local provider locations. It
imports Codex `~/.codex/history.jsonl` through the same explicit prompt-only
`summary_only` path when present, reports missing Codex history otherwise, and
reports discovered Claude or Pi directories as unsupported native hooks instead
of importing unproven transcripts. Provider-native hooks are not implemented in
this branch. See [provider-support.md](provider-support.md) for the current
support matrix and native-history blockers.

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
ctx publish pr-comment <record-id> --dry-run
ctx publish pr-comment <record-id> --dry-run --include-raw-transcript
ctx publish pr-comment <record-id>
```

- `link-pr` stores a local pull request URL string on a Work Record. Use
  `ctx pr parse` first to validate and normalize the URL before attaching it.
- `publish pr-comment --dry-run` renders the finished-product PR comment
  Markdown for the linked GitHub pull request without mutating the network.
- `publish pr-comment` uses the authenticated `gh` CLI to create or update one
  marker-bounded ctx comment on the linked GitHub pull request.
- PR comment rendering redacts command output and secret-like content by
  default.
  `--include-raw-transcript` is an explicit opt-in for private PRs where raw
  command stdout/stderr evidence is acceptable to share; publishing currently
  supports GitHub PRs through `gh`.
- GitLab publishing and hosted/team publishing remain outside this local
  launch scope.

## Export, import, and validate

```bash
ctx export
ctx export --output work-records.json
ctx import --input work-records.json
cat work-records.json | ctx import
ctx validate
ctx doctor
ctx doctor --privacy
ctx repair
ctx repair --json
```

- `export` writes a JSON archive to stdout or `--output`, including local blob
  payloads needed to preserve evidence output.
- `import` reads a JSON archive from `--input` or stdin.
- `import` handles ctx JSON archives only; provider inputs use
  `ctx capture import-provider` or the Codex prompt-history path above.
- `validate` checks local Work Recorder storage and prints `valid` when no findings are found.
- `doctor` runs the same local health checks using the product-facing command name.
- `doctor --privacy` prints local-only storage posture, validation state,
  capture spool counts, and filesystem permission status for the Work Recorder
  directory, database, and inbox.
- `repair` retries failed capture spool files and imports anything that succeeds.

Normal Work Recorder commands import pending capture spool files before serving
results. Failed imports are retained as `.failed` files for inspection and
retry.

## Not yet implemented

This branch does not include hosted sync, passive provider hooks beyond local
Git/jj/gh wrapper shims, native Claude/Pi transcript import, public installer
flow, hosted/team Option A, or hosted/team publishing; hosted/team pull request
publishing remains outside launch.
