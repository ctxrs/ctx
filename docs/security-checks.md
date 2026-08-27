# Security Checks

This page defines the checks public docs and validation should keep true for
the local retrieval product.

## Required Invariants

- `ctx setup` reads supported provider history and writes only under the
  configured ctx data root: Core/Tantivy generations, optional semantic data,
  config data, and optional persistent daemon lock/status/job state when
  automatic autostart runs. Manual setup starts no worker.
- `ctx sources` listing writes nothing in local-only security mode.
  `ctx sources add [--replace]` and `ctx sources remove` write only the locked,
  durably replaced `config.toml`; they never modify provider history.
- `ctx import` writes only under the configured ctx data root: Core generations,
  optional semantic data, config data, and optional daemon lock/status/job
  state when a persistent daemon or finite Core worker runs.
- Automatic background search and explicit search `--refresh wait` may request
  a bounded daemon-owned refresh of discovered native provider history before
  querying the active Core generation. Manual background search and
  `--refresh off` must not start or wake a process. The query process does not
  write Core generations or projections. Without semantic opt-in, default
  search must not download embedding models or start semantic indexing.
- `ctx show` writes nothing in local-only security mode, except
  `ctx show session --out` writes only the explicit path when one is provided.
- `ctx status` does not mutate canonical history: missing stores stay missing,
  and existing stores are not migrated, repaired, or used to create search
  generations. It reports daemon and supervisor health without changing either.
- `ctx index` is read-only, as are `ctx index mode` with no mode argument,
  `ctx index watch`, and `ctx index wait`. `ctx index mode auto` and
  `ctx index mode manual` are explicit configuration and process-lifecycle
  mutations. They persist the requested mode and reconcile supervision to the
  effective mode; a process-level override can keep manual mode active after an
  auto request.
- In local-only security mode, setup/import/default search do not use network
  access or API keys. Explicit semantic use still must not call hosted model
  APIs, and search must not download the local embedding model when the required
  cache is missing. Explicit semantic/hybrid search may initialize an
  already-cached local model to embed the query.
- `ctx setup --no-daemon` and `ctx import --no-daemon` must not autostart daemon
  maintenance or finite workers. Machine-readable output is not a process-start
  security control. The deprecated `ctx setup --catalog-only` flag is ignored and is not
  a daemon-autostart security control either.
- `ctx docs` reads embedded documentation and writes only an explicit topic
  output path for `ctx docs show --out` or an explicit man-page output
  directory when `ctx docs man --out` is used.
- `ctx upgrade` uses signed release metadata with explicit self-upgrade policy
  and applies only to official installer-managed binaries with a matching
  install sidecar. Production metadata origin, detached-signature derivation,
  public key, and artifact-origin prefix are binary constants; ambient config
  and environment cannot replace them, and release-related child processes
  remove inherited release-authority variables before execution.
- Public release construction emits one atomic authority handoff. Its aggregate
  Windows candidate manifest binds the exact construction executable, release
  checksum file, runtime archive, and runtime DLL. The public verifier accepts
  that exact handoff plus an independently supplied expected manifest digest;
  it does not sign, attest, or treat a matching handoff sidecar as authority.
- Automatic upgrade defaults on for managed installs. Automatic indexing with
  the full daemon profile uses the enabled persistent daemon as the sole check
  and apply driver. Manual indexing, source-refresh-only mode, ordinary
  foreground commands, MCP, and finite Core workers perform no automatic check,
  download, or apply. One installation-scoped scheduler and lock coordinates
  daemon and explicit upgrade work. Signed policy and explicit opt-outs remain
  mandatory, and upgrade work must not collect provider history or pollute
  command stdout/stderr.

- A ctx-owned persistent coordinator, when launched by `ctx daemon run` or
  automatic setup/import autostart, must write only under the configured ctx
  data root, respect `[indexing] mode` unless explicitly forced, and may run
  only bounded native local provider-history refresh and bounded semantic
  catch-up. It must not run history-source plugins.
  Network model acquisition is allowed only for the local embedding model when
  semantic search is explicitly enabled with `ctx setup --semantic` in auto
  mode. `ctx daemon run` blocks in the foreground and does not mutate indexing
  mode.
- A finite Core worker may start only for explicit import or search
  `--refresh wait`. It must not install persistent supervision or run watcher,
  timer, semantic, or upgrade maintenance, and it must not exit before admitted
  Core refresh work is terminal and its IPC endpoint is quiescent.
- Provider files are read as sources and not modified.
- Provider-native SQLite histories are opened as short read-only logical
  snapshots; ctx does not checkpoint, migrate, or write those databases.
- Provider transcript imports reject symlinked JSONL files by default.
- JSON output is private by default.
- Search/show JSON preserves local transcript text by default, including
  absolute paths and secret-shaped strings. It must be treated as private local
  data.
- The public provider support matrix contains only supported providers and uses
  only the `supported` status. Unsupported-provider rationale is outside the
  public support matrix.
- Every support row is exercised by a deterministic public locate, import-all,
  user-and-assistant search/show/citation, and unchanged-repeat contract test.

## Static Docs Checks

Public docs should avoid claims for capabilities outside the product contract.
Run the repository docs check, which scans public copy for removed or unsupported
product surfaces:

```bash
bash scripts/check-docs.sh
```

Validate the provider matrix JSON:

```bash
jq empty docs/provider-support-matrix.json
```

When Bazel owns the docs gate, run:

```bash
scripts/bazelw test //:docs_check --config=ci
```

## Bazel Security Gates

Run the affected public integration selection after search/show/locate changes:

```bash
scripts/bazel-affected.sh origin/main
```

The owning search/show/locate oracle imports synthetic provider history with
fake secret-shaped values and checks that those commands preserve the
documented private local-data boundary.

## Mode Placement

Security-sensitive product changes should run the focused owning targets and
`bash scripts/check.sh --mode=ci` as described in
[`docs/testing-taxonomy.md`](testing-taxonomy.md).

The default retrieval boundary remains local provider-history search. Security
docs and tests should continue to reject claims that setup, import, search, or
doctor need remote accounts, provider-history background collection,
repository mutation, or API keys.

## Manual Review Checklist

- README scope matches `docs/product-contract.md`.
- CLI examples use flags implemented by `crates/ctx-cli`.
- Provider support docs match `docs/provider-support-matrix.json`.
- Testing taxonomy keeps the public command surface focused on local search and
  static smoke coverage.
- JSON docs identify local/private output.
- Symlink policy stays explicit: provider transcript symlinks are rejected unless
  a future change adds canonical root-contained symlink support with tests.
- Security docs do not promise default local sanitization.
- Public docs do not make strict no-network claims except when describing
  local-only security mode.
