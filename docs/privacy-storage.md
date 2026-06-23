# Privacy and Storage

ctx is local-first. Work records start on the machine where the CLI is
running, and the public `0.1.0` candidate keeps that boundary explicit.

## Privacy defaults

Default review surfaces are share-safe by design:

- `ctx list`, `ctx show`, `ctx search`, `ctx context`, and `ctx report` redact
  secret-like values, credential URLs, and local filesystem paths;
- `ctx dashboard export` writes a local React/Vite dashboard with bundled local
  assets only and no hosted sync or remote tracking;
- `ctx publish pr-comment --dry-run` renders the exact PR comment locally before
  any network mutation;
- raw transcript or evidence payload sharing is withheld by default.

`ctx publish pr-comment --include-raw-transcript` is an explicit opt-in for
private review workflows where sharing raw command output is acceptable. That
flag should not be treated as the default review path.

Redaction is heuristic. It covers known secret-like patterns, credential URLs,
and local paths used by the current tests, but it is not a general-purpose
sanitizer for arbitrary terminal output, provider transcripts, archives, local
SQLite rows, or object payloads. Treat all raw records, raw archives,
`objects/`, and `spool/` entries as private even when the default review views
look clean.

## Local data

By default, ctx stores local data directly under `~/.ctx`, or under the root
named by `CTX_DATA_ROOT`/`--data-root`. That root is the ctx root itself; ctx
does not append an extra product directory. The canonical local layout is:

```text
~/.ctx/
  work.sqlite
  objects/
  spool/
  shims/
  config.toml
  logs/
```

The recorder stores structured metadata in SQLite:

- records
- prompts and notes
- record timestamps
- tags, kinds, and optional workspace paths
- command evidence metadata
- safe previews of command stdout and stderr captured by `ctx evidence run` or
  imported local shims
- pull request URLs attached by `ctx link-pr`
- provider import summary data for the sources this branch can prove

The current implementation stores records and command evidence metadata in the
local SQLite database at `work.sqlite`. Full command stdout and stderr are
stored as content-addressed local-only object files under `objects/`, with
SQLite rows pointing at those artifacts. Export and import use JSON archives
that include the object payloads needed to preserve recorded evidence on
another machine. Shims live under `shims/`, capture envelopes queue in
`spool/`, configuration lives in `config.toml`, and diagnostics belong in
`logs/`.

The current implementation does not store passive provider transcripts,
dashboard state, or hosted sync state. Explicit provider fixture imports and
the explicit local Codex prompt-history JSONL import are stored only when the
user runs those commands. Local Git/jj/gh shim events are written to the JSONL
capture spool and imported only into local storage.

Hosted accounts, hosted sync, team policy, hosted dashboards, organization
analytics, hosted retention, and hosted publish workflows are not in launch
scope for this branch. See [hosted-sync-roadmap.md](hosted-sync-roadmap.md) for
the future direction without turning it into a shipped claim.

`ctx setup` does not install a persistent service by default. Local recording
works through commands and shims without a daemon; `ctx setup --service` and
`ctx service install` are explicit opt-ins.

Provider transcript import or hook expansion must be treated as a privacy
boundary change. Before public docs claim a broader provider path, the matching
worker needs provider-specific redaction tests, malformed-input tests, raw
retention notes, and threat-model updates for the new source format.

## Capture spool

`ctx capture import` reads pending JSONL capture envelope files from fixtures
or opt-in shims in the local ctx spool. These files may contain
prompts, command output, paths, and tool metadata before import. Successful
imports are renamed to `.done`; failed imports are renamed to `.failed` and get
an `.error.json` sidecar.

Treat the spool and imported JSON archives as sensitive local data. This branch
does not install provider-native hooks. Spool files are created by local
tooling, fixture workflows, or the Git/jj/gh wrapper shims installed by
`ctx setup` after their directory is active on `PATH`.

Do not treat pending or failed spool entries as share-safe just because the
eventual review output is redacted. A failed entry can still contain raw command
output, paths, prompts, provider metadata, or partial JSONL that was never
normalized.

## What may be sensitive

Work records can contain:

- source code pasted into record bodies or command output
- proprietary prompts
- agent summaries pasted into record bodies
- shell commands and paths
- command output with secrets or customer data
- private repository and pull request links

Treat the ctx data directory like source code. Do not publish it unless the
record has been reviewed for sensitive content. Raw JSON archives and the local
data root remain private data even when the share-safe review surfaces look
clean.

The redaction corpus fixture in [redaction-corpus.md](redaction-corpus.md)
documents synthetic examples covered by review-output tests.

## Network behavior

The local recorder is useful without network sync. This branch does not include
hosted sync or account login. `ctx publish pr-comment` can publish a local
GitHub PR comment through the authenticated `gh` CLI; hosted/team publishing is
out of scope. Exported JSON archives can include full command output payloads
and should be reviewed before they leave your machine.

Agent providers, package managers, GitHub, and other tools you run during the
task may still use the network according to their own configuration. ctx stores
the local records and command evidence you explicitly create around those tools;
it does not make those tools private by itself.

## Retention

Keep records as long as they help review, audit, handoff, or debugging. Remove local recorder data that is no longer needed or that contains data you should not retain.

Recommended habits:

- record only evidence that helps explain the work
- prefer redacted command output when full output contains secrets
- review exported records before sharing
- inspect pending or failed capture spool files before sharing logs
- keep the data directory out of public repos
- remove old local recorder data on shared machines when it is no longer needed

## Portability

SQLite plus JSON export keeps records inspectable and portable. A record should be useful even if the agent provider, model, or terminal session that produced the work is gone.

See [threat-model.md](threat-model.md) for the launch security boundary,
[provider-support.md](provider-support.md) for proven provider surfaces, and
[troubleshooting.md](troubleshooting.md) for first-line local triage.
