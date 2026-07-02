# Changelog

This file tracks public ctx CLI releases. Dates use the release source commit
date. The latest stable installer is documented at <https://ctx.rs/install>.

## 0.16.0 - 2026-07-02

Custom history sources for agent formats ctx does not read natively.

- Added `ctx-history-jsonl-v1` imports for explicit JSONL interchange files.
- Added local history-source plugins: a manifest plus script or binary command
  that streams `ctx-history-jsonl-v1` records to stdout.
- Taught `ctx sources`, `ctx import`, `ctx search`, and MCP search/source flows
  to use custom history sources, source filters, stored cursors, and pre-search
  refresh.
- Documented the plugin contract in the repository, embedded docs, README, and
  ctx.rs site.
- Fixed a plugin timeout edge case where helper processes could inherit stdout
  or stderr and keep a command waiting past the configured source timeout.

Source: [f78a0973](https://github.com/ctxrs/ctx/commit/f78a0973f0b7fd971af0f2d690ac2e31dca25af0)

## 0.15.0 - 2026-07-01

Native import for more personal-agent history.

- Added first-class local history import and search support for OpenClaw,
  Hermes, NanoClaw, and AstrBot.
- Updated provider docs and fixture coverage for the new native sources.
- Cleaned up the new provider code before release.

Source: [b4280575](https://github.com/ctxrs/ctx/commit/b42805757e85b31f1b951fbbd839b02e33424525)

## 0.14.0 - 2026-07-01

SDK groundwork and release hardening.

- Added experimental in-repo agent-history SDKs while keeping them
  non-publishing.
- Cross-built macOS CLI artifacts from Linux with pinned Zig and
  `cargo-zigbuild` tooling.
- Hardened release and archive coverage.
- Cleaned up a clippy fixture issue in store archive-validation tests.

Source: [b0d938aa](https://github.com/ctxrs/ctx/commit/b0d938aa45cd3375548f28029ca98247d5a26a4e)

## 0.13.0 - 2026-07-01

Search defaults and provider polish.

- Changed `ctx search` to exclude subagent sessions by default, with
  `--include-subagents` for explicit subagent coverage.
- Cleaned up search filter state and legacy JSON fields.
- Linked touched-file metadata through the stable SQL views.
- Added embedded docs for SQL, MCP, and upgrade topics, with better weak-match
  suppression and recovery suggestions.
- Clarified provider display names and provider filter docs.
- Added progress output for `ctx doctor`.
- Made Darwin CLI artifact generation work from Linux by using
  `cargo-zigbuild`.

Source: [bad3cace](https://github.com/ctxrs/ctx/commit/bad3cace3ed578199d90bf014cfcf3ea12208260)

## 0.12.0 - 2026-06-30

Read-only SQL over the local ctx store.

- Added `ctx sql` for one bounded, read-only SQL statement over the local
  store.
- Added the MCP `sql` tool for advanced agent queries.
- Added stable read-only views: `ctx_sessions`, `ctx_events`,
  `ctx_files_touched`, and `ctx_sources`.
- Supported SQL input from an argument, stdin, or `--file`.
- Added execution bounds for rows, columns, SQL size, value size, SQLite
  allocation, and timeouts.

Source: [74bb09cf](https://github.com/ctxrs/ctx/commit/74bb09cfb8ca4f1dcc23b2f6c5e810b83566ecd9)

## 0.11.0 - 2026-06-30

Managed upgrades and built-in docs.

- Added signed managed upgrade checks and apply flow through `ctx upgrade`.
- Added background auto-upgrade checks for hosted-installer-managed installs.
- Added built-in documentation through `ctx docs`.
- Added generated man pages through `ctx docs man`.
- Updated the hosted Unix and PowerShell installers to verify signed CLI
  metadata, write managed install markers, and install generated Unix man pages.
- Added explicit self-upgrade and auto-upgrade policy flags to release metadata.

Source: [9a38a12a](https://github.com/ctxrs/ctx/commit/9a38a12a5c5b5c9fcdb3b05318e2b29bd8811641)

## 0.10.0 - 2026-06-30

Touched-file search and local/private storage cleanup.

- Added touched-file metadata ingestion where provider transcripts expose file
  paths through tool calls, patches, commands, or native fields.
- Added `ctx search --file <path>` examples and JSON contract notes for
  touched-file matches and citations.
- Stopped describing search output as share-safe redacted text. ctx stores and
  searches local/private transcript text; copied output should be reviewed
  before sharing.
- Added an upgrade reindex path so older data roots can refresh derived search
  projections and re-read original provider transcripts when they still exist.
- Added the README token-efficiency chart and refreshed agent skill docs.

Source: [1bdd9943](https://github.com/ctxrs/ctx/commit/1bdd9943fe76be648514f66cf93587ac176cfa15)

## 0.8.0 - 2026-06-29

Simpler public command surface.

- Removed redundant top-level `ctx list`, `ctx export`, and `ctx validate`
  commands.
- Kept `ctx doctor` as the storage health command.
- Moved transcript file writing to `ctx show session --out`.
- Updated docs, JSON contracts, security notes, and installed agent
  instructions for the smaller command surface.

Source: [7331158b](https://github.com/ctxrs/ctx/commit/7331158b180493c0fcf19026cda51172e9d5306f)

## 0.7.0 - 2026-06-29

Research moved out of the CLI surface.

- Removed the public top-level `ctx research` command and MCP `research` tool.
- Kept history research as an agent workflow composed from `ctx search`, scoped
  `ctx search --session`, and `ctx show`.
- Updated docs and agent skill instructions to use the composable commands.

Source: [70df37e4](https://github.com/ctxrs/ctx/commit/70df37e4055bc27d8c0b47b2849ca61f9115d8a6)

## 0.6.0 - 2026-06-29

Search and MCP usability pass.

- Added the read-only MCP stdio server.
- Made default search output more compact and action-oriented, including
  inspect commands for follow-up retrieval.
- Defaulted session transcript rendering toward lite output.
- Sped up source counting in `ctx status`.
- Synced installed agent skill/plugin instructions with the refined CLI flow.

Source: [9a005c87](https://github.com/ctxrs/ctx/commit/9a005c87c10d3a843dd53474f3000f504447b41f)

## 0.5.0 - 2026-06-27

Maintenance after the first public release.

- Moved indexed history item counting into the store so setup/search checks no
  longer had to enumerate every session and event.
- Avoided doing that count unless search had no results and needed a useful
  next step.
- Updated package versions and public artifact checks for the release.

Source: [c7d95fcf](https://github.com/ctxrs/ctx/commit/c7d95fcfa6aecd1aef05f512ba94e60457896408)

## 0.4.0 - 2026-06-26

Incremental refresh for search.

- Made search refresh incrementally import discovered native provider sources.
- Preserved Codex tail state through recataloging.
- Avoided global FTS rebuilds during normal refresh.
- Added regression and performance coverage for no-op and tail-refresh paths.

Source: [abc08a15](https://github.com/ctxrs/ctx/commit/abc08a1558769c6cade174537e1e55ee10cd5e37)

## 0.3.0 - 2026-06-26

Source discovery and import maturity.

- Improved provider source discovery and importability reporting for
  `ctx sources`.
- Added clearer refresh controls and freshness reporting around search.
- Added Codex incremental import performance coverage.
- Tightened hosted installer setup and search behavior.

Source: [5cf4f497](https://github.com/ctxrs/ctx/commit/5cf4f49704626024198b32cd345564f3cc370d71)

## 0.2.0 - 2026-06-25

First stable hosted CLI release.

- Shipped the local SQLite index for agent-history sessions and events.
- Supported setup, source discovery, import, search, show, locate, doctor, and
  JSON output for agent workflows.
- Included native local-history imports for the first supported coding-agent
  formats.
- Published hosted installers and cross-platform CLI artifacts.

Source: [22b94fe3](https://github.com/ctxrs/ctx/commit/22b94fe3c76eece7836c5b528e9f9e463b421943)
