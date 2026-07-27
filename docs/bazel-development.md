# Bazel Development

Bazel is the authoritative Rust build and test graph. Cargo manifests and
`Cargo.lock` remain dependency metadata for `crate_universe`; they do not define
a second supported build path.

## Fast Linux loop

Use the repository wrapper so every worktree receives its own Bazel output
base while repository downloads and action outputs are shared:

```bash
scripts/bazelw build //crates/ctx-cli:ctx --config=dev-linux
scripts/bazelw test //crates/ctx-history-search:unit_tests --config=dev-linux
scripts/bazelw test //crates/ctx-protocol:unit_tests //crates/ctx-sdk:unit_tests --config=dev-linux
scripts/bazelw test //:native_rust_smoke --config=dev-linux
```

The two SDK unit-test targets above are the authoritative Rust SDK test path;
the language-agnostic contract and non-Rust SDK checks remain in
`//:sdk_contract_checks`.

`dev-linux` uses `fastbuild`, Rust `debuginfo=0`, and the content-pinned mold
archive declared in `MODULE.bazel`. `dev-debug` deliberately has a different
key, enables debug information, and uses the platform linker. `test`, `ci`, and
`release` likewise have distinct configuration keys.

The wrapper defaults to `${XDG_CACHE_HOME:-$HOME/.cache}/ctx/bazel`. On a
machine with shared NVMe storage, point all worktrees at the same cache root:

```bash
export CTX_BAZEL_CACHE_ROOT=/mnt/shared/ctx-bazel
scripts/bazelw test //:native_rust --config=test
```

The layout is:

- `output-roots/<workspace-hash>`: one output-user-root per canonical worktree
- `repository-cache`: shared immutable downloads
- `action-cache`: shared content-addressed action results

Ephemeral sandboxes default to the system temporary directory and can be
moved with `CTX_BAZEL_SANDBOX_BASE`. CI may set `BAZEL_OUTPUT_USER_ROOT`
explicitly while retaining the same repository and action caches.

## Complete and affected checks

```bash
scripts/check.sh --mode=fast
scripts/check.sh --mode=presubmit
scripts/check.sh --mode=ci
scripts/bazel-affected.sh origin/main
```

The affected command uses pinned bazel-diff, a detached cached base worktree,
and complete-content hashes for both graphs. BUILD, `.bzl`, module, lock, and
configuration changes select the full presubmit suite. A diff/query/filter
failure or a changed file with no mapped test also fails closed to presubmit.
Targets tagged `manual` or `external-harness` stay outside routine execution.

`tools/bazel/rust-target-inventory.json` records native ownership for every
Cargo production, binary, example, build-script, and integration-test target.
Its test fails whenever a manifest target is added without a Bazel label.

## Platform boundary

The checked-in native host toolchain currently covers x86-64 Linux. The target
inventory models release inputs, but native Windows, macOS, Linux AArch64, and
FreeBSD execution still requires the corresponding registered Bazel Rust/C++
toolchains and native runners. Existing platform release jobs remain the
explicit manual limitation until those toolchains are registered; they must
not be treated as evidence that a host-Cargo build is authoritative.
