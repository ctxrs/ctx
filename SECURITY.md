# Security Policy

ctx is currently a local-first CLI for work records. The launch branch does
not include hosted sync, hosted accounts, team policy enforcement, public
installer URLs, hosted publishing, or GitLab publishing.

## Supported Surface

Security review for this branch covers the local ctx recording surface:

- local data root at `${CTX_DATA_ROOT:-~/.ctx}` with no extra product
  directory appended;
- SQLite metadata in `work.sqlite`, local object payloads in `objects/`,
  durable capture envelopes in `spool/`, wrapper shims in `shims/`,
  `config.toml`, and local logs in `logs/`;
- explicit `ctx record`, `ctx evidence run`, export/import, search, report, and
  dashboard export commands;
- opt-in local Git/jj/gh wrapper shims;
- pull request URL parsing and local `ctx link-pr`;
- dry-run and live GitHub PR comment publishing through the authenticated local
  `gh` CLI, using one marker-bounded ctx-owned comment.

Normalized provider fixture import, Codex prompt-history import, and explicit
Pi session import are in scope with the limitations documented in
`docs/provider-support.md`. Broad provider-native transcript importers,
provider-native shell hooks, hosted team workflows, and hosted publish commands
remain product direction unless the CLI reference documents a shipped command.

## Reporting Vulnerabilities

Do not publish private prompts, command output, customer data, credentials, or
local record archives in a public issue. Report vulnerabilities through the
project's private security reporting channel when available. If a private
channel is not available for the repository you are using, contact a maintainer
before sharing reproducer data.

Useful reports include:

- affected command or data flow;
- ctx version or commit;
- operating system;
- whether `CTX_DATA_ROOT` was set;
- a minimal redacted reproducer;
- expected and observed behavior.

## Local Data Handling

Treat the ctx data root and exported archives as sensitive local data. They may
contain source code, prompts, paths, command output, pull request links, and
secrets that appeared in terminal output.

The current branch does not upload ctx work records by itself. Networked tools
run by the user, such as package managers, Git remotes, agent providers, and
GitHub CLI, keep their own network behavior and security model.

`ctx setup` does not install a persistent service by default. Recording works
through local commands and shims without a daemon. A background service is
opt-in through `ctx setup --service` or `ctx service install`.

## Redaction and Raw Data Limits

ctx review surfaces use heuristic redaction for secret-shaped values,
credential URLs, and local paths. That redaction is a safety layer for default
review output, not a general-purpose sanitizer for arbitrary provider
transcripts, terminal output, archives, or local object payloads.

Raw transcript content and full stdout/stderr object payloads are private local
data by default. Sharing them outside the machine must remain an explicit
opt-in, such as a reviewed export path or `ctx publish pr-comment
--include-raw-transcript`. Do not document broad raw transcript sync, raw
object upload, or provider transcript sharing as a default behavior.

Before adding or expanding provider transcript import, provider-native hooks,
new capture writers, hosted sync, or new publish targets, the implementation
needs matching redaction tests, provider-specific malformed-input tests, and
threat-model coverage for the new data flow. Unsupported or fixture-only
provider fidelity must stay explicit in public docs until provider and release
workers land code and CI evidence for a stronger claim.

## Security Documentation

- [Threat model](docs/threat-model.md)
- [Privacy and storage](docs/privacy-storage.md)
- [Redaction corpus](docs/redaction-corpus.md)
- [Dependency and license audit decisions](docs/dependency-license-audit.md)
