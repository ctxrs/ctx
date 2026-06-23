# Privacy and Storage

ctx is local-first. Work Records start on the machine where the CLI is running.

## Local data

The recorder stores structured metadata in SQLite:

- records
- prompts and notes
- record timestamps
- tags, kinds, and optional workspace paths
- command evidence metadata
- safe previews of command stdout and stderr captured by `ctx evidence run` or
  imported local shims
- pull request URLs attached by `ctx link-pr`

The current implementation stores records and command evidence metadata in the
local SQLite database. Full command stdout and stderr are stored as
content-addressed local-only blob files under the Work Recorder data directory,
with SQLite rows pointing at those artifacts. Export and import use JSON
archives that include the blob payloads needed to preserve recorded evidence on
another machine.

The current implementation does not store passive provider transcripts,
dashboard state, or hosted sync state. Local Git/jj/gh shim events are written
to the JSONL capture inbox and imported only into local storage.

Hosted accounts, hosted sync, team policy, hosted dashboards, organization
analytics, hosted retention, and hosted publish workflows are not in launch
scope for this branch.

## Capture inbox

`ctx capture import` reads pending JSONL capture envelope files from the local
Work Recorder inbox. These files may contain prompts, command output, paths, and
tool metadata before import. Successful imports are renamed to `.done`; failed
imports are renamed to `.failed` and get an `.error.json` sidecar.

Treat the inbox and imported JSON archives as sensitive local data. This branch
does not install provider hooks or shell hooks. Inbox files are created by local
tooling, fixture workflows, or the opt-in Git/jj/gh wrapper shims that
explicitly write them.

## What may be sensitive

Work Records can contain:

- source code pasted into record bodies or command output
- proprietary prompts
- agent summaries pasted into record bodies
- shell commands and paths
- command output with secrets or customer data
- private repository and pull request links

Treat the ctx data directory like source code. Do not publish it unless the record has been reviewed for sensitive content.

The redaction corpus fixture in [redaction-corpus.md](redaction-corpus.md)
documents synthetic examples that future redaction tests should cover. It is
not a guarantee that every current output path is automatically redacted.

## Network behavior

The local recorder is useful without network sync. This branch does not include
hosted sync, account login, or pull request comment publishing. Exported JSON
archives can include full command output payloads and should be reviewed before
they leave your machine.

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

See [threat-model.md](threat-model.md) for the launch security boundary and
remaining code-security follow-ups.
