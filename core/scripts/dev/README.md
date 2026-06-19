# Local Development Validation

Use `scripts/dev/cargo-safe.sh` for local Rust builds and tests that are run by
agents or broad verification scripts. It adds the repo's host-level safety
policy around Cargo:

- one Cargo invocation at a time per host through `/tmp/ctx-cargo.lock`;
- conservative Cargo jobs and Rust test threads;
- optional systemd user-scope memory limits when available;
- low I/O priority and `nice` by default;
- AppImage environment cleanup before running host Cargo.

Avoid launching direct concurrent `cargo test`, `cargo build`, `cargo check`, or
Rust Bazel builds from multiple local agents on the same machine. `-j 1` limits
one Cargo process, but it does not protect the host when several agents each
start their own Cargo command. The lock is cooperative: direct `cargo` and Rust
Bazel commands can bypass it, so local agents should route broad Rust validation
through this wrapper.

Useful overrides:

```bash
CTX_CARGO_MEMORY_MAX_GIB=24 \
CTX_CARGO_JOBS=1 \
CTX_RUST_TEST_THREADS=1 \
scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --locked -p ctx-http
```

Set `CTX_CARGO_LOCK=0` to disable the global lock, `CTX_CARGO_LOW_IO=0` to
disable `ionice`/`nice`, or `CTX_CARGO_LOCK_PATH=/path/to/lock` to share a
custom semaphore across workspaces.
