# Security Policy

ctx Work Recorder is currently a local-first CLI. The launch branch does not
include hosted sync, hosted accounts, team policy enforcement, public installer
URLs, or pull request comment publishing.

## Supported Surface

Security review for this branch covers the local Work Recorder surface:

- local data root under `${CTX_DATA_ROOT:-~/.ctx}/work-record/`;
- SQLite metadata, local blob artifacts, and capture inbox files;
- explicit `ctx record`, `ctx evidence run`, export/import, search, report, and
  dashboard export commands;
- opt-in local Git/jj/gh wrapper shims;
- pull request URL parsing and local `ctx link-pr`.

Provider transcript importers, shell hooks, hosted team workflows, and hosted
publish commands are product direction unless the CLI reference documents a
shipped command.

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

Treat the Work Recorder data root and exported archives as sensitive local
data. They may contain source code, prompts, paths, command output, pull
request links, and secrets that appeared in terminal output.

The current branch does not upload Work Recorder data by itself. Networked
tools run by the user, such as package managers, Git remotes, agent providers,
and GitHub CLI, keep their own network behavior and security model.

## Security Documentation

- [Threat model](docs/threat-model.md)
- [Privacy and storage](docs/privacy-storage.md)
- [Redaction corpus](docs/redaction-corpus.md)
- [Dependency and license audit decisions](docs/dependency-license-audit.md)
