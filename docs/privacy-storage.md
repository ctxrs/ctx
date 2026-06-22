# Privacy and Storage

ctx is local-first. Work Records start on the machine where the CLI is running.

## Local data

The recorder stores structured metadata in SQLite:

- records
- prompts and notes
- record timestamps
- tags, kinds, and optional workspace paths
- command evidence metadata
- command stdout and stderr captured by `ctx evidence run`
- pull request URLs attached by `ctx link-pr`

The current implementation stores records and command evidence in the local SQLite database. Export and import use JSON archives.

The current implementation does not store passive provider transcripts, shim
events, dashboard state, hosted sync state, or blob files.

## What may be sensitive

Work Records can contain:

- source code pasted into record bodies or command output
- proprietary prompts
- agent summaries pasted into record bodies
- shell commands and paths
- command output with secrets or customer data
- private repository and pull request links

Treat the ctx data directory like source code. Do not publish it unless the record has been reviewed for sensitive content.

## Network behavior

The local recorder is useful without network sync. This branch does not include
hosted sync, account login, or pull request comment publishing. Exported JSON
archives should be reviewed before they leave your machine.

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
- keep the data directory out of public repos
- remove old local recorder data on shared machines when it is no longer needed

## Portability

SQLite plus JSON export keeps records inspectable and portable. A record should be useful even if the agent provider, model, or terminal session that produced the work is gone.
