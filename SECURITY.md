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
- setup, status, sources, import, show, locate, search, MCP, and doctor output;
- JSON output treated as private local data unless reviewed before sharing.

Setup, source discovery, import, and search do not require API keys,
repository writes, shell startup-file edits, or background processes.
No session text, prompts, or transcripts leave this machine by default.
When local-only security mode is enabled, these commands also do not use
network access.

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
- a minimal sanitized reproducer;
- expected and observed behavior.

## Local Data Handling

Treat the ctx data root and command output as sensitive. They may contain source
code, prompts, local paths, command output previews, private repository names,
and secrets that appeared in provider transcripts.

On Unix, ctx creates or repairs its managed data directories and core index,
lock, object, and capture-spool files with owner-only permissions. On Windows,
ctx currently relies on ACLs inherited from the chosen data directory and does
not apply an owner-only ACL itself. Choose a data root whose inherited Windows
ACL is limited to the intended account.

Raw provider transcript files remain in provider-owned locations. ctx imports
the searchable text and metadata it needs into SQLite, so deleting or moving the
raw transcript does not necessarily remove indexed text from ctx. Delete the ctx
data root or rebuild the index when local retention requirements change.

## Local Output Limits

Search, show, SQL, MCP, and JSON output are local/private by default and may
preserve local paths, token-shaped strings, command output, and other transcript
text when that text exists in indexed payloads. Review copied output before
sharing it outside the machine.

Before adding a new provider importer or expanding stored fields, the change
needs tests for malformed input, source-path handling, local payload handling,
and the no-network/no-repository-write behavior required by local-only security
mode.

## Security Documentation

- [Threat model](docs/threat-model.md)
- [Security checks](docs/security-checks.md)
- [Storage and privacy](docs/storage.md)
