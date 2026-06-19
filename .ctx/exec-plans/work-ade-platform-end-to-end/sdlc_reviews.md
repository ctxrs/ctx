# SDLC Reviews

Record process, worktree, validation, and agent-workflow reviews.

## Pending

- Initial SDLC review after Phase 0 commit hygiene.
- Final SDLC review before full local validation.

## Plan Review Baseline

- The execution plan now requires manager-owned contract commits before broad
  parallel worker fan-out.
- Worker handoffs must include base commit, diff stat, invariants changed, tests
  run, residual risks, expected conflicts, and integration notes.
- Heavy validation remains serialized and memory-capped.

## Local Cargo Pressure Review

- Finding: multiple agents running independent local Cargo commands can freeze
  this host even when each command uses `-j 1`, because linking large Rust test
  binaries still creates root-drive write pressure and `-j 1` is only
  per-process.
- Process rule: broad local Rust validation must go through
  `core/scripts/dev/cargo-safe.sh`, which now owns a host-level Cargo lock and
  low-I/O priority policy. Agents must not run direct concurrent Cargo commands
  for broad verification.
- Host-specific mitigations such as APST, scheduler, dirty writeback, and
  `cargo-lowio` are useful on this machine, but the repo process must not depend
  on that host setup existing.
- Reviewer Lagrange (`019ee177-56e3-7520-b1a8-3304e456f331`) found no blockers
  in the wrapper change. The manager addressed the non-blocking concerns by
  probing `ionice` before using it and documenting that the lock is cooperative,
  so direct Cargo/Bazel can still bypass it.

## Contract Base

- Broad implementation workers must base on `8123c74` unless the manager records
  a later contract base.
