# Security Checks

This page defines the checks public docs and validation should keep true for
the search-only product.

## Required Invariants

- `ctx setup` creates only the configured ctx data root and local storage files.
- `ctx sources` writes nothing in local-only security mode.
- `ctx import` writes only the configured ctx data root and SQLite index.
- `ctx search`, `ctx list`, and `ctx show` write nothing in local-only
  security mode.
- In local-only security mode, setup/import/search do not use network access or
  API keys.
- In the side-effect oracle and local-only security mode, analytics are
  disabled by env so the core no-network invariant is strict.
- With default user config, first-party analytics are disabled. When explicitly
  enabled, analytics may create `install.json` and send coarse CLI invocation
  metadata.
- Provider files are read as sources and not modified.
- Provider transcript imports reject symlinked JSONL files by default.
- JSON output is private by default and must not be described as share-safe.
- Search/show JSON and SQLite search projections must not expose
  secret-shaped values that the redaction oracle covers.
- Unsupported providers remain explicit in the provider support matrix.

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
bazel test //:docs_check --config=ci
```

## Bazel Security Gates

Run the public privacy/redaction oracle through Bazel:

```bash
bazel test //:privacy_redaction_oracle --config=ci
```

`//:privacy_redaction_oracle` imports a synthetic provider history with fake
secret-shaped values, then checks `search`, `show`, and SQLite search
projections for redaction.

## Mode Placement

Security-sensitive product changes should run at least `presubmit`; changes to
setup/import/search behavior should also run `smoke` as described in
[`docs/testing-taxonomy.md`](testing-taxonomy.md).

The default product boundary remains local search only. Security docs and tests
should continue to reject claims that setup, import, search, doctor, or validate
need remote accounts, background processes, repository mutation, or API keys.

## Manual Review Checklist

- README scope matches `docs/product-contract.md`.
- CLI examples use flags implemented by `crates/ctx-cli`.
- Provider support docs match `docs/provider-support-matrix.json`.
- Testing taxonomy keeps the public command surface focused on local search and
  static smoke coverage.
- JSON docs identify local/private output and compatibility limits.
- Symlink policy stays explicit: provider transcript symlinks are rejected unless
  a future change adds canonical root-contained symlink support with tests.
- Security docs do not promise sanitization beyond bounded previews and
  share-safety markers.
