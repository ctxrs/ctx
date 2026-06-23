# Dependency and License Audit Decisions

This branch keeps dependency and license review explicit because Work Recorder
handles sensitive local records and command output.

## Current Decision

For the local launch branch:

- workspace crates declare `Apache-2.0`;
- third-party Rust dependencies are managed through Cargo and pinned by
  `Cargo.lock`;
- no public installer or auto-updater is documented as live;
- no hosted service dependency is required for local Work Recorder commands;
- a formal vulnerability/license gate is required before public installer URLs,
  hosted sync, or update commands are documented as shipped.

The current docs check is lightweight and does not replace dependency scanning.

## Dependency Inventory

The direct dependency set is intentionally small and centered on CLI parsing,
serialization, local storage, timestamps, ids, hashing, regex/search, URLs, and
test helpers. Notable supply-chain-sensitive dependencies include:

- `rusqlite` with the `bundled` feature, which builds SQLite as part of the Rust
  dependency graph;
- `clap` for CLI argument parsing;
- `serde` and `serde_json` for archive and capture envelope parsing;
- `regex` and `url` for matching and URL parsing;
- `uuid`, `sha2`, and `chrono` for ids, fingerprints, and timestamps.

## Launch Gate Before Public Distribution

Before documenting public installer URLs or updater behavior, run and record:

- `cargo metadata --locked` dependency inventory;
- vulnerability scan such as `cargo audit` or an approved equivalent;
- license scan such as `cargo deny` or an approved equivalent;
- review of build scripts and bundled native code;
- release artifact provenance, checksum, and signature plan;
- installer script review for TLS, shell safety, and channel pinning.

## Accepted Local-Only Risk

The branch can remain source-build only while this audit gate is incomplete.
That is an intentional launch decision: local source builds are useful for
dogfood, while public installer/update trust requires a stronger release
process.

## Follow-Ups

- Add `cargo-deny` or equivalent policy when dependency policy is finalized.
- Add CI jobs for vulnerability and license scanning.
- Document release signing and checksum verification.
- Decide whether bundled SQLite is preferred over system SQLite per platform.
