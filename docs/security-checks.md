# Security Checks

This page defines the checks public docs and validation should keep true for
the local retrieval product.

## Required Invariants

- `ctx setup` reads supported provider history and writes only under the
  configured ctx data root: Core/Tantivy generations, optional semantic data,
  config data, and optional daemon lock/status/job state when daemon autostart
  runs.
- `ctx sources` writes nothing in local-only security mode.
- `ctx import` writes only under the configured ctx data root: Core generations,
  optional semantic and Pro-derived state, config data, and optional daemon
  lock/status/job state when daemon autostart runs.
- `ctx search` may request a bounded daemon-owned refresh of discovered native
  provider history before querying the active Core generation. The query process
  does not write Core generations or projections. Without semantic opt-in,
  default search must not download embedding models or start semantic indexing.
- `ctx show` writes nothing in local-only security mode, except
  `ctx show session --out` writes only the explicit path when one is provided.
- `ctx status` does not mutate canonical history or local Pro graph data:
  missing stores stay missing, and existing stores are not migrated, repaired,
  or used to create search generations. Pro entitlement authorization may
  advance nonsecret anti-clock-rollback security metadata.
- In local-only security mode, setup/import/default search do not use network
  access or API keys. Explicit semantic use still must not call hosted model
  APIs, and search must not download the local embedding model when the required
  cache is missing. Explicit semantic/hybrid search may initialize an
  already-cached local model to embed the query.
- `ctx setup --no-daemon` and `ctx import --no-daemon` must not autostart daemon
  maintenance. Machine-readable output is not a daemon-autostart security
  control. The deprecated `ctx setup --catalog-only` flag is ignored and is not
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
- Automatic upgrade defaults on for managed installs, but the enabled daemon is
  its only scheduler. A disabled daemon performs no automatic check, download,
  or apply. Signed policy and explicit opt-outs remain mandatory, and upgrade
  work must not collect provider history or pollute command stdout/stderr.

- A ctx-owned background coordinator, when launched by `ctx daemon run` or
  setup/import autostart, must write only under the configured ctx data root,
  respect `[daemon].enabled` unless explicitly forced, and may run only bounded
  native local provider-history refresh and bounded semantic catch-up. It must
  not run history-source plugins.
  Network model acquisition is allowed only for the local embedding model when
  semantic search is explicitly enabled.
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
`//:ci` as described in
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

## No-Native-Store Combined-Candidate Gate

The public and private release candidates must be qualified together at their
reviewed commit SHAs. A mixed candidate is not evidence for this contract.
Run the following sequence natively on every available Linux, macOS, and
Windows release platform with the native credential adapter made genuinely
unavailable:

1. On a fresh canonical root, verify read-only credential and graph-key
   inspection creates nothing; then start an anonymous trial and verify the
   public credential and private graph-key namespaces independently select
   their sticky owner-private file backends.
2. Import canonical NativePath history, materialize Core plus Pro, restart the daemon,
   and run `ctx blame`; verify Core Tantivy retrieval never depends on either
   credential namespace and the SQLCipher graph remains derived-facts,
   source-rebuildable state.
3. Upgrade the same root to the candidate pair, restart the daemon and helper,
   and repeat materialization and blame. Verify neither namespace changes its
   selected backend when a native vault later becomes available.
4. Run verified `ctx pro uninstall --delete-data`, including an interrupted
   deletion retry. Verify public records and public-owned empty backend state
   are absent, and verify the selected private graph-key record and graph data
   are absent without changing the private selector's existing sticky
   lifecycle or crossing either namespace's ownership boundary.
5. Repeat mutation attempts with native selections already durable while the
   vault is locked, denied, corrupt, ambiguous, canceled, missing access or
   entitlement, or unavailable. Every case must fail closed without creating a
   file-backend selector or record. macOS fallback eligibility is limited to
   `errSecNotAvailable` and no default keychain; Windows eligibility is limited
   to `ERROR_NO_SUCH_LOGON_SESSION`.

Linux additionally reruns the existing no-session-bus, process/thread race,
owner-mode, symlink, hardlink, bounded-record, corruption, sticky-selection,
and verified-deletion regression suite. macOS qualification checks owner-only
mode, rejects extended ACLs, and checks file-and-directory sync behavior where
supported. Windows qualification checks a protected current-user-only DACL,
not POSIX mode, and rejects reparse points and unexpected links.

Cross-compilation and hermetic mocked-adapter tests are required supporting
evidence, but never replace a native platform pass. If a native runner is
unavailable, the release record names the infrastructure blocker and leaves
that platform gate open.
