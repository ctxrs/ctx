# Changelog

This file tracks public ctx CLI releases. Dates use the release source commit
date. PR links appear when GitHub has a merged PR for the shipped source;
direct release work is linked as commits.

The latest stable installer is documented at <https://ctx.rs/install>.

## 0.16.0 - 2026-07-02

### Added

- Added `ctx-history-jsonl-v1` imports for explicit JSONL interchange files.
- Added local history-source plugins: a manifest plus script or binary command
  that streams `ctx-history-jsonl-v1` records to stdout.
- Added custom history-source filters and cursor handoff to CLI and MCP
  source/search flows.

### Changed

- `ctx sources`, `ctx import`, `ctx search`, and MCP source/search flows now
  understand custom history sources.
- Search-time refresh can run enabled custom history plugins before querying.
- Plugin docs are linked from the README, embedded docs, and ctx.rs site.
- README and import-source UX were tightened after the 0.15 release.

### Fixed

- Fixed plugin timeout handling when helper descendants keep stdout or stderr
  open past the configured source timeout.
- Improved the error for nonexistent import paths.

### Commits and PRs

- Source commit: [f78a0973](https://github.com/ctxrs/ctx/commit/f78a0973f0b7fd971af0f2d690ac2e31dca25af0)
- Full diff: [b4280575...f78a0973](https://github.com/ctxrs/ctx/compare/b42805757e85b31f1b951fbbd839b02e33424525...f78a0973f0b7fd971af0f2d690ac2e31dca25af0)
- Merged PRs: [#23](https://github.com/ctxrs/ctx/pull/23) ([eb48bc15](https://github.com/ctxrs/ctx/commit/eb48bc153e57587caf93c5e6739e1e950bfdb574), [92d10122](https://github.com/ctxrs/ctx/commit/92d10122292fc6a664b7ca056a3493ce39920b4d), merge [9b5e5b2f](https://github.com/ctxrs/ctx/commit/9b5e5b2fa9b9cbdbee70276e80c620dfe395d905)); [#24](https://github.com/ctxrs/ctx/pull/24) ([374a46c5](https://github.com/ctxrs/ctx/commit/374a46c5a03a47f720f4ff1c14eca094cb86b08e), merge [60315348](https://github.com/ctxrs/ctx/commit/60315348f98cf7b6347e1628a1e1420c139f80c5)); [#25](https://github.com/ctxrs/ctx/pull/25) ([4ac09671](https://github.com/ctxrs/ctx/commit/4ac0967193fa65b72097857cdd323f682b114c98), merge [ba5eb393](https://github.com/ctxrs/ctx/commit/ba5eb393d10173630e76961488ffced87f4dd31c)).
- Direct commits: [f7e2f8cb](https://github.com/ctxrs/ctx/commit/f7e2f8cb5e78ead27dc5419e819dcff478b2e5a6), [a1af9c7c](https://github.com/ctxrs/ctx/commit/a1af9c7c865fbe82bcaeac91f17e7ffea56ba4c9), [ced549f4](https://github.com/ctxrs/ctx/commit/ced549f46dbd2b40351c78cec265901a6e43e66f), [78cd4b46](https://github.com/ctxrs/ctx/commit/78cd4b46a9995447b0973df29cdcd9444a75d1d9), [4dde2d96](https://github.com/ctxrs/ctx/commit/4dde2d962dc8eea435218f102d64d5fc1e490595), [10cfd711](https://github.com/ctxrs/ctx/commit/10cfd7113483a3c177d237bc878298efcb948390), [f78a0973](https://github.com/ctxrs/ctx/commit/f78a0973f0b7fd971af0f2d690ac2e31dca25af0).

## 0.15.0 - 2026-07-01

### Added

- Added first-class local history import and search support for OpenClaw,
  Hermes, NanoClaw, and AstrBot.

### Changed

- Updated provider docs and fixture coverage for the new native sources.
- Bumped the CLI/runtime crates to `0.15.0`.

### Fixed

- Cleaned up provider-code clippy issues before release.

### Commits and PRs

- Source commit: [b4280575](https://github.com/ctxrs/ctx/commit/b42805757e85b31f1b951fbbd839b02e33424525)
- Full diff: [b0d938aa...b4280575](https://github.com/ctxrs/ctx/compare/b0d938aa45cd3375548f28029ca98247d5a26a4e...b42805757e85b31f1b951fbbd839b02e33424525)
- Merged PRs: [#21](https://github.com/ctxrs/ctx/pull/21) ([84bd1d24](https://github.com/ctxrs/ctx/commit/84bd1d249bde35ad48611b1b45662c7d549d08f6), merge [7d71efd7](https://github.com/ctxrs/ctx/commit/7d71efd756c6d0b63f31b54d7aabe3c58fc4ca22)); [#22](https://github.com/ctxrs/ctx/pull/22) ([6df3f7ae](https://github.com/ctxrs/ctx/commit/6df3f7ae623ba2752e563ade68844be75043b9df), merge [b4280575](https://github.com/ctxrs/ctx/commit/b42805757e85b31f1b951fbbd839b02e33424525)).

## 0.14.0 - 2026-07-01

### Added

- Added experimental in-repo agent-history SDKs while keeping them
  non-publishing.
- Added privacy-safe telemetry device identity support.

### Changed

- Cross-built macOS CLI artifacts from Linux with pinned Zig and
  `cargo-zigbuild` tooling.
- Hardened release and archive coverage.
- Renamed SDK contracts and docs around the agent-history naming.

### Fixed

- Fixed Swift SDK full-toolchain tests.
- Cleaned up a clippy fixture issue in store archive-validation tests.

### Commits and PRs

- Source commit: [b0d938aa](https://github.com/ctxrs/ctx/commit/b0d938aa45cd3375548f28029ca98247d5a26a4e)
- Full diff: [bad3cace...b0d938aa](https://github.com/ctxrs/ctx/compare/bad3cace3ed578199d90bf014cfcf3ea12208260...b0d938aa45cd3375548f28029ca98247d5a26a4e)
- Merged PRs: [#20](https://github.com/ctxrs/ctx/pull/20) ([42ab67f5](https://github.com/ctxrs/ctx/commit/42ab67f571fbf85817dce57f8e39eb9eca017c36), merge [c3bffbdc](https://github.com/ctxrs/ctx/commit/c3bffbdc2a9cba40ce93a15a7725027e08678bc4)).
- Direct commits: [6ba82519](https://github.com/ctxrs/ctx/commit/6ba82519f9ac8aba914970d6dcd981a366dabab5), [be42c1a6](https://github.com/ctxrs/ctx/commit/be42c1a6301c909d7422fa1dbcfcb5e7b1ed17cf), [71c3b3c9](https://github.com/ctxrs/ctx/commit/71c3b3c9c3ac271e69e1946fd3148db06a59503a), [782b8e32](https://github.com/ctxrs/ctx/commit/782b8e32d6b2fa32c719b98662eaa9ed117674b5), [d02c30fa](https://github.com/ctxrs/ctx/commit/d02c30fa81c5042dcff8e398a1ff5a8afc4cb587), [647d7889](https://github.com/ctxrs/ctx/commit/647d7889d0a404ca7ebf3ca02a66e174cc2b3165), [0540c26e](https://github.com/ctxrs/ctx/commit/0540c26e9fd56d01d715bfe66ddb9df7645fa7ec), [93a0c10b](https://github.com/ctxrs/ctx/commit/93a0c10b9af0687c30cdf713eea136b5320d6955), [b0d938aa](https://github.com/ctxrs/ctx/commit/b0d938aa45cd3375548f28029ca98247d5a26a4e).

## 0.13.0 - 2026-07-01

### Added

- Added embedded docs for SQL, MCP, and upgrade topics.
- Added `ctx doctor` progress output.
- Added richer agent skill guidance for advanced ctx workflows.

### Changed

- `ctx search` excludes subagent sessions by default, with
  `--include-subagents` for explicit subagent coverage.
- Search filter state and legacy JSON output were cleaned up.
- Provider display names and provider filter docs were clarified.
- Darwin CLI artifact generation can run from Linux through `cargo-zigbuild`.

### Fixed

- Fixed catalog import checkpoint state.
- Kept checkpoint helper tests clippy-clean.
- Suppressed weak embedded-doc search matches and added recovery suggestions.

### Commits and PRs

- Source commit: [bad3cace](https://github.com/ctxrs/ctx/commit/bad3cace3ed578199d90bf014cfcf3ea12208260)
- Full diff: [74bb09cf...bad3cace](https://github.com/ctxrs/ctx/compare/74bb09cfb8ca4f1dcc23b2f6c5e810b83566ecd9...bad3cace3ed578199d90bf014cfcf3ea12208260)
- Merged PRs: [#17](https://github.com/ctxrs/ctx/pull/17) ([81ba338a](https://github.com/ctxrs/ctx/commit/81ba338a13a6a86ede095dead00c4f261d7651f1), merge [22c2fde6](https://github.com/ctxrs/ctx/commit/22c2fde6ab8759118c71aea3accc4a41c87af767)).
- Direct commits: [d737d2dd](https://github.com/ctxrs/ctx/commit/d737d2ddebdd0b63df09bdd50a687749f4552be7), [010666b8](https://github.com/ctxrs/ctx/commit/010666b8ab873f60f478467322756725430c540f), [3effe403](https://github.com/ctxrs/ctx/commit/3effe4037503631d7e5afb571d30048a25402350), [bf0c0319](https://github.com/ctxrs/ctx/commit/bf0c0319b5bb709f16dcbe4f4e6b47cc6e37b04b), [9b7a68b5](https://github.com/ctxrs/ctx/commit/9b7a68b5d43a53e4a0957f4303f7402b93b8fb3e), [48038596](https://github.com/ctxrs/ctx/commit/480385960c6312bc0e6552f26daf3cdca450d801), [0d1838db](https://github.com/ctxrs/ctx/commit/0d1838dbc257636824f18a842e478d42276871d1), [b7c77800](https://github.com/ctxrs/ctx/commit/b7c778003c65ba0e03de934a3f025ee6342275f8), [ebafca46](https://github.com/ctxrs/ctx/commit/ebafca4633eaa2b79bf84bb8d6e728230e02f3cf), [87c543c6](https://github.com/ctxrs/ctx/commit/87c543c68511f82acfe83352c59ea952a4b31822), [e6886030](https://github.com/ctxrs/ctx/commit/e6886030e8a2a5bd72c59257957304e12eac9b7a), [3d1282d6](https://github.com/ctxrs/ctx/commit/3d1282d6411b949493a0d00d55fd3c43a4b065c7), [f71d114c](https://github.com/ctxrs/ctx/commit/f71d114c180c4262d0d77592103ddd4ee05ac1a6), [34d641d0](https://github.com/ctxrs/ctx/commit/34d641d0b4b72a442b8f9f8393a7f75d9ac2fbdd), [f7775be9](https://github.com/ctxrs/ctx/commit/f7775be9793058cf8a852490569e5e54f0df8fd0), [572ba143](https://github.com/ctxrs/ctx/commit/572ba1438d3200c008231c67248d9b478b0a3525), [bad3cace](https://github.com/ctxrs/ctx/commit/bad3cace3ed578199d90bf014cfcf3ea12208260).

## 0.12.0 - 2026-06-30

### Added

- Added `ctx sql` for one bounded, read-only SQL statement over the local
  store.
- Added the MCP `sql` tool for advanced agent queries.
- Added stable read-only views: `ctx_sessions`, `ctx_events`,
  `ctx_files_touched`, and `ctx_sources`.
- Added SQL input from an argument, stdin, or `--file`.

### Changed

- SQL execution is bounded by rows, columns, SQL size, value size, SQLite
  allocation, and timeout limits.

### Commits and PRs

- Source commit: [74bb09cf](https://github.com/ctxrs/ctx/commit/74bb09cfb8ca4f1dcc23b2f6c5e810b83566ecd9)
- Full diff: [9a38a12a...74bb09cf](https://github.com/ctxrs/ctx/compare/9a38a12a5c5b5c9fcdb3b05318e2b29bd8811641...74bb09cfb8ca4f1dcc23b2f6c5e810b83566ecd9)
- Direct commits: [74bb09cf](https://github.com/ctxrs/ctx/commit/74bb09cfb8ca4f1dcc23b2f6c5e810b83566ecd9).

## 0.11.0 - 2026-06-30

### Added

- Added signed managed upgrade checks and apply flow through `ctx upgrade`.
- Added background auto-upgrade checks for hosted-installer-managed installs.
- Added built-in documentation through `ctx docs`.
- Added generated man pages through `ctx docs man`.

### Changed

- Hosted Unix and PowerShell installers verify signed CLI metadata, write
  managed install markers, and install generated Unix man pages.
- Release metadata carries explicit self-upgrade and auto-upgrade policy flags.

### Commits and PRs

- Source commit: [9a38a12a](https://github.com/ctxrs/ctx/commit/9a38a12a5c5b5c9fcdb3b05318e2b29bd8811641)
- Full diff: [1bdd9943...9a38a12a](https://github.com/ctxrs/ctx/compare/1bdd9943fe76be648514f66cf93587ac176cfa15...9a38a12a5c5b5c9fcdb3b05318e2b29bd8811641)
- Direct commits: [9a38a12a](https://github.com/ctxrs/ctx/commit/9a38a12a5c5b5c9fcdb3b05318e2b29bd8811641).

## 0.10.0 - 2026-06-30

### Added

- Added touched-file metadata ingestion where provider transcripts expose file
  paths through tool calls, patches, commands, or native fields.
- Added `ctx search --file <path>` examples and JSON contract notes for
  touched-file matches and citations.
- Added the README token-efficiency chart.

### Changed

- Search output is described as local/private transcript text, not share-safe
  redacted text.
- Older data roots can refresh derived search projections and re-read original
  provider transcripts when they still exist.
- Agent skill docs were refreshed.

### Fixed

- Reverted the experimental ctx web search UI before release.

### Commits and PRs

- Source commit: [1bdd9943](https://github.com/ctxrs/ctx/commit/1bdd9943fe76be648514f66cf93587ac176cfa15)
- Full diff: [7331158b...1bdd9943](https://github.com/ctxrs/ctx/compare/7331158b180493c0fcf19026cda51172e9d5306f...1bdd9943fe76be648514f66cf93587ac176cfa15)
- Direct commits: [337d055b](https://github.com/ctxrs/ctx/commit/337d055b2c1623b1198d16a76f0fa54fe8e3d0e7), [e97ffc8b](https://github.com/ctxrs/ctx/commit/e97ffc8b8de18fd20d5c9ed232570bdf325b91f7), [94053dc5](https://github.com/ctxrs/ctx/commit/94053dc5166bca2557d9fdaeca35cf2c1bb7599a), [6d5c674f](https://github.com/ctxrs/ctx/commit/6d5c674f53419649f2a7dca1ad0dcb9868c542fa), [140f8821](https://github.com/ctxrs/ctx/commit/140f8821cffa81e56d2039b8a7046102b60e86e6), [3dffbf8b](https://github.com/ctxrs/ctx/commit/3dffbf8b0dee22f8319ccf522f5f557798241994), [d304c2f2](https://github.com/ctxrs/ctx/commit/d304c2f2d23118ef4eae6c69a2f399c49cfcb036), [8fcbf589](https://github.com/ctxrs/ctx/commit/8fcbf5896597a36526464b88cb167c078ce66aaf), [1bdd9943](https://github.com/ctxrs/ctx/commit/1bdd9943fe76be648514f66cf93587ac176cfa15).

## 0.8.0 - 2026-06-29

### Changed

- Removed redundant top-level `ctx list`, `ctx export`, and `ctx validate`
  commands.
- Kept `ctx doctor` as the storage health command.
- Moved transcript file writing to `ctx show session --out`.
- Updated docs, JSON contracts, security notes, and installed agent
  instructions for the smaller command surface.

### Commits and PRs

- Source commit: [7331158b](https://github.com/ctxrs/ctx/commit/7331158b180493c0fcf19026cda51172e9d5306f)
- Full diff: [70df37e4...7331158b](https://github.com/ctxrs/ctx/compare/70df37e4055bc27d8c0b47b2849ca61f9115d8a6...7331158b180493c0fcf19026cda51172e9d5306f)
- Direct commits: [7331158b](https://github.com/ctxrs/ctx/commit/7331158b180493c0fcf19026cda51172e9d5306f).

## 0.7.0 - 2026-06-29

### Changed

- Removed the public top-level `ctx research` command and MCP `research` tool.
- Kept history research as an agent workflow composed from `ctx search`, scoped
  `ctx search --session`, and `ctx show`.
- Updated docs and agent skill instructions to use the composable commands.

### Commits and PRs

- Source commit: [70df37e4](https://github.com/ctxrs/ctx/commit/70df37e4055bc27d8c0b47b2849ca61f9115d8a6)
- Full diff: [9a005c87...70df37e4](https://github.com/ctxrs/ctx/compare/9a005c87c10d3a843dd53474f3000f504447b41f...70df37e4055bc27d8c0b47b2849ca61f9115d8a6)
- Direct commits: [70df37e4](https://github.com/ctxrs/ctx/commit/70df37e4055bc27d8c0b47b2849ca61f9115d8a6).

## 0.6.0 - 2026-06-29

### Added

- Added the read-only MCP stdio server.
- Added deterministic research packets and performance gates.
- Added real-corpus search quality benchmarks.

### Changed

- Default search output became more compact and action-oriented, including
  inspect commands for follow-up retrieval.
- Session transcript rendering defaults toward lite output.
- `ctx status` source counting is faster.
- Installed agent skill/plugin instructions were synced with the refined CLI
  flow.

### Fixed

- Satisfied strict clippy for the CLI UX changes.
- Kept session metrics scoped to session-search results.

### Commits and PRs

- Source commit: [9a005c87](https://github.com/ctxrs/ctx/commit/9a005c87c10d3a843dd53474f3000f504447b41f)
- Full diff: [c7d95fcf...9a005c87](https://github.com/ctxrs/ctx/compare/c7d95fcfa6aecd1aef05f512ba94e60457896408...9a005c87c10d3a843dd53474f3000f504447b41f)
- Direct commits: [4e826ca6](https://github.com/ctxrs/ctx/commit/4e826ca6394b598e1963a0d892febe7b8b9c56fc), [80a79424](https://github.com/ctxrs/ctx/commit/80a7942444a1613614a05d67f8078c29611de658), [adb4db50](https://github.com/ctxrs/ctx/commit/adb4db50713d12b25d53c8ebe2f935fd2b9f44ff), [f12e22af](https://github.com/ctxrs/ctx/commit/f12e22af094afdcc0da80a12d6888067e7881d01), [d53fe7cc](https://github.com/ctxrs/ctx/commit/d53fe7cc9dde2aa0591f3acb71930552ede67f4c), [ab62d57f](https://github.com/ctxrs/ctx/commit/ab62d57f0e05ef615893204466d502b345e60df7), [81cda0d1](https://github.com/ctxrs/ctx/commit/81cda0d12b600cd271480db2a8361d81d6888f07), [363a7c97](https://github.com/ctxrs/ctx/commit/363a7c9743330de11e78f9093acab59cfb23447e), [0ff9f14d](https://github.com/ctxrs/ctx/commit/0ff9f14d98df9c6568aa3239d398fd06dd1234b0), [c653149a](https://github.com/ctxrs/ctx/commit/c653149a844cfb80908db25a5f01418a8d9a6c3a), [a7f4386f](https://github.com/ctxrs/ctx/commit/a7f4386f7dcbf0658fa5c5ba6e9ffb97cd23a887), [9a005c87](https://github.com/ctxrs/ctx/commit/9a005c87c10d3a843dd53474f3000f504447b41f).

## 0.5.0 - 2026-06-27

### Changed

- Moved indexed history item counting into the store so setup/search checks no
  longer had to enumerate every session and event.
- Avoided doing that count unless search had no results and needed a useful
  next step.
- Updated package versions and public artifact checks for the release.

### Commits and PRs

- Source commit: [c7d95fcf](https://github.com/ctxrs/ctx/commit/c7d95fcfa6aecd1aef05f512ba94e60457896408)
- Branch diff: [8c55c2ca...c7d95fcf](https://github.com/ctxrs/ctx/compare/8c55c2cad5f40aaf504cdbfd6904443f47bd86f7...c7d95fcfa6aecd1aef05f512ba94e60457896408)
- Direct commits: [8c55c2ca](https://github.com/ctxrs/ctx/commit/8c55c2cad5f40aaf504cdbfd6904443f47bd86f7), [eb6a460a](https://github.com/ctxrs/ctx/commit/eb6a460a84510e5d0cc2cf41e2bf0a6bb7e16a1c), [c7d95fcf](https://github.com/ctxrs/ctx/commit/c7d95fcfa6aecd1aef05f512ba94e60457896408).

## 0.4.0 - 2026-06-26

### Changed

- Made search refresh incrementally import discovered native provider sources.
- Preserved Codex tail state through recataloging.
- Avoided global FTS rebuilds during normal refresh.

### Fixed

- Added regression and performance coverage for no-op and tail-refresh paths.

### Commits and PRs

- Source commit: [abc08a15](https://github.com/ctxrs/ctx/commit/abc08a1558769c6cade174537e1e55ee10cd5e37)
- Full diff: [5cf4f497...abc08a15](https://github.com/ctxrs/ctx/compare/5cf4f49704626024198b32cd345564f3cc370d71...abc08a1558769c6cade174537e1e55ee10cd5e37)
- Direct commits: [abc08a15](https://github.com/ctxrs/ctx/commit/abc08a1558769c6cade174537e1e55ee10cd5e37).

## 0.3.0 - 2026-06-26

### Added

- Added clearer provider source discovery and importability reporting for
  `ctx sources`.
- Added Codex incremental import performance coverage.

### Changed

- Improved search refresh controls and freshness reporting.
- Tightened hosted installer setup and search behavior.

### Fixed

- Fixed an incremental performance clippy lint.

### Commits and PRs

- Source commit: [5cf4f497](https://github.com/ctxrs/ctx/commit/5cf4f49704626024198b32cd345564f3cc370d71)
- Full diff: [22b94fe3...5cf4f497](https://github.com/ctxrs/ctx/compare/22b94fe3c76eece7836c5b528e9f9e463b421943...5cf4f49704626024198b32cd345564f3cc370d71)
- Direct commits: [d738d919](https://github.com/ctxrs/ctx/commit/d738d9196e9db40309ae54eee18e65ce4b9bc113), [0920d005](https://github.com/ctxrs/ctx/commit/0920d005c3c25296f24e394aa49d8448ea1ef46d), [5cf4f497](https://github.com/ctxrs/ctx/commit/5cf4f49704626024198b32cd345564f3cc370d71).

## 0.2.0 - 2026-06-25

### Added

- Shipped the local SQLite index for agent-history sessions and events.
- Supported setup, source discovery, import, search, show, locate, doctor, and
  JSON output for agent workflows.
- Included native local-history imports for the first supported coding-agent
  formats.
- Published hosted installers and cross-platform CLI artifacts.

### Fixed

- Fixed OpenCode import sequencing before the stable metadata refresh.

### Commits and PRs

- Source commit: [22b94fe3](https://github.com/ctxrs/ctx/commit/22b94fe3c76eece7836c5b528e9f9e463b421943)
- Notable commits: [dc90632a](https://github.com/ctxrs/ctx/commit/dc90632ab2ca56d6ce6f22bd1d5e5b5d65a65473), [22b94fe3](https://github.com/ctxrs/ctx/commit/22b94fe3c76eece7836c5b528e9f9e463b421943).
