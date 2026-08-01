# Security Policy

ctx is a local CLI for indexing and searching existing agent history. The
security boundary for this branch is the local machine, the configured ctx data
root, and provider transcript files the user explicitly imports or allows ctx
to discover.

## Supported Surface

Security review for the current product covers:

- the `ctx` CLI commands documented in `docs/cli-reference.md`;
- the default data root `${CTX_DATA_ROOT:-~/.ctx}`;
- the immutable Core/Tantivy generations in `search/lexical`, the optional
  flat-F32 semantic projection in `search/semantic`, and metadata-only
  `relational.sqlite`;
- local `config.toml` and diagnostic logs when present;
- read-only discovery of known provider history paths;
- explicit imports for supported local transcript formats, including Codex,
  Pi, Claude, OpenCode, Gemini, Cursor, Copilot CLI, and Factory AI Droid;
- setup, status, sources, import, show, search, MCP, and doctor output;
- JSON output treated as private local data unless reviewed before sharing.

Setup, source discovery, import, and search do not require API keys,
repository writes, shell startup-file edits, or background processes.
No session text, prompts, or transcripts leave this machine by default.
When local-only security mode is enabled, these commands also do not use
network access.

## macOS Release Signing Identity

Official macOS release binaries beginning with ctx 0.25.0 are signed and
notarized under this identity:

- Developer ID authority:
  `Developer ID Application: Legacy Publisher LLC (SJSNARH4TG)`
- Apple Team ID: `SJSNARH4TG`

On macOS, verify an installed official release binary's integrity and Apple
trust, then inspect its signing identity with:

```bash
codesign --verify --strict --verbose=4 "$(command -v ctx)"
spctl --assess --verbose=4 --type install "$(command -v ctx)"
codesign -d --verbose=4 "$(command -v ctx)" 2>&1 | grep -E '^(Authority|TeamIdentifier)='
```

The relevant output is:

```text
Authority=Developer ID Application: Legacy Publisher LLC (SJSNARH4TG)
Authority=Developer ID Certification Authority
Authority=Apple Root CA
TeamIdentifier=SJSNARH4TG
```

Legacy Publisher LLC is the legacy legal name still attached to ctx
engineering, inc.'s Apple Developer account. Both names identify the same
publisher; Team ID `SJSNARH4TG` is the stable identifier to compare. Until
Apple reflects the current legal name, macOS may show “Legacy Publisher
Institute LLC” in Login Items or background-activity notices. Source builds and
package-manager builds are not necessarily the official signed release bytes;
run the commands against a downloaded official macOS release binary when
checking one of those installation paths.

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
code, prompts, local paths, tool-call arguments, private repository names, and
typed identifiers extracted from provider transcripts.

Provider transcript files remain provider-owned acquisition inputs. Import and
daemon refresh publish policy-selected normalized content and metadata into
self-contained Core generations. Search and show read the active verified Core
generation without reopening provider transcripts at query time. Acquisition
paths remain source-level discovery and import metadata; `show` does not return
them. A temporarily inaccessible input fails that source's refresh without being
treated as confirmed deletion, while the active Core generation remains
queryable until a later refresh publishes new source state.

## Local Output Limits

Search, show, SQL, MCP, and JSON output are local/private by default and may
contain Core-backed transcript text, local paths, token-shaped strings, command
output, and other transcript data. Review copied output before sharing it
outside the machine.

Before adding a new provider importer or expanding stored fields, the change
needs tests for malformed input, source-path handling, local payload handling,
and the no-network/no-repository-write behavior required by local-only security
mode.

## Security Documentation

- [Threat model](docs/threat-model.md)
- [Security checks](docs/security-checks.md)
- [Storage and privacy](docs/storage.md)
