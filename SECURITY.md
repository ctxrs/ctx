# Security Policy

ctx is a local CLI for indexing and searching existing agent history. The
security boundary for this branch is the local machine, the configured ctx data
root, and provider transcript files the user explicitly imports or allows ctx
to discover.

## Supported Surface

Security review for the current product covers:

- the `ctx` CLI commands documented in `docs/cli-reference.md`;
- the default data root `${CTX_DATA_ROOT:-~/.ctx}`;
- SQLite metadata and searchable text in `work.sqlite`;
- local `config.toml` and diagnostic logs when present;
- read-only discovery of known provider history paths;
- explicit imports for supported local transcript formats, including Codex,
  Pi, Claude, OpenCode, Gemini, Cursor, Copilot CLI, and Factory AI Droid;
- search, list, show, status, doctor, and validate output;
- JSON output treated as private local data unless reviewed and redacted.

Setup, source discovery, import, and search do not require API keys,
repository writes, shell startup-file edits, or background processes.
First-party analytics are disabled by default and send only when explicitly
enabled. In local-only security mode, analytics are disabled and these commands
do not use network access.

## Reporting Vulnerabilities

Do not publish private prompts, command output, customer data, credentials, raw
transcripts, SQLite databases, or local archives in a public issue. Use the
project's private security reporting channel when available. If no private
channel is available for the repository you are using, contact a maintainer
before sharing reproducer data.

Useful reports include:

- affected command or data flow;
- ctx version or commit;
- operating system;
- whether `CTX_DATA_ROOT` or `--data-root` was set;
- provider and source format, if relevant;
- a minimal redacted reproducer;
- expected and observed behavior.

## Local Data Handling

Treat the ctx data root and command output as sensitive. They may contain source
code, prompts, local paths, command output previews, private repository names,
and secrets that appeared in provider transcripts.

Raw provider transcript files remain in provider-owned locations. ctx imports
the searchable text and metadata it needs into SQLite, so deleting or moving the
raw transcript does not necessarily remove indexed text from ctx. Delete the ctx
data root or rebuild the index when local retention requirements change.

## Redaction Limits

ctx applies bounded previews and share-safety markers in search and show output,
but these are guardrails, not a general-purpose sanitizer. JSON output is
local/private by default. Review and redact copied output before sharing it
outside the machine.

Before adding a new provider importer or expanding stored fields, the change
needs tests for malformed input, source-path handling, redaction boundaries, and
the local-only no-network/no-repository-write behavior described in the docs.

## Security Documentation

- [Threat model](docs/threat-model.md)
- [Security checks](docs/security-checks.md)
- [Storage and privacy](docs/storage.md)
- [Redaction corpus](docs/redaction-corpus.md)
