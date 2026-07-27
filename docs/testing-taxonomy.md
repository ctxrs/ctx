# Testing Taxonomy

Public verification focuses on fast local confidence for the search CLI.

## Modes

| Mode | Purpose |
| --- | --- |
| `fast` | Native Rust formatting/smoke, target inventory, public docs, CLI contracts, and package-surface audit. |
| `smoke` | `fast` plus a fresh-home CLI flow and basic provider fixture smoke. |
| `presubmit` | `fast` plus the complete native Rust unit and integration graph. |
| `ci` | Buildkite gate: native clippy, `presubmit`, SDK packaging, and release/content audit. |

## Commands

```bash
bash scripts/check.sh --mode=fast
bash scripts/check.sh --mode=smoke
bash scripts/check.sh --mode=presubmit
bash scripts/check.sh --mode=ci
```

Use direct Bazel targets when a narrower check is enough:

```bash
scripts/bazelw test //:docs_check --config=test
scripts/bazelw test //:fresh_home_e2e --config=test
scripts/bazelw test //:provider_fixture_e2e --config=test
scripts/bazelw test //:local_transcript_oracle --config=test
scripts/bazelw test //:package_audit_release --config=release
```

All default public tests must be hermetic. They must not require API keys,
network access, provider accounts, hidden model calls, or writes into source
repositories.

## Upgrade Compatibility

Importer identity, cursor, dedupe-key, and source-root changes must be tested as
upgrades, not only as fresh imports. A regression test for such a change starts
from the oldest relevant stored record shape, opens it through the current
schema migrations, changes or appends provider content, and imports again. It
must prove that logical session and event counts remain stable, existing ctx IDs
still resolve, genuinely new events are retained once, and cross-source
sessions remain distinct.

Keep these upgrade fixtures sanitized and hermetic. When a release changes a
stored identity input, add the compatibility test in the same change; a
fresh-home idempotency test alone cannot prove migration safety.

When identity code is shared across providers, cover each distinct historical
storage shape established by the compatibility audit rather than duplicating
the same test for every provider label.
