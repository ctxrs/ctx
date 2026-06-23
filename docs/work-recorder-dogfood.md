# Work Recorder Dashboard Dogfood

Use the dashboard review dogfood script to produce a local, deterministic set of
finished-product artifacts for visual review. The script writes only under the
configured `CTX_DATA_ROOT` and artifact directory, and the defaults stay inside
`target/`.

```bash
scripts/dashboard-review-dogfood.sh
```

Default outputs are written to `target/ctx-artifacts/dashboard-review`:

- `dashboard/index.html` is the static dashboard export.
- `report.txt` and `report.json` are CLI report outputs for quick count checks.
- `context.md` and `search.json` exercise handoff/search review paths.
- `work-records.json` is the exported archive from the seeded local store.
- `manifest.json` lists artifact paths and screenshot status.
- `screenshots/desktop.png` and `screenshots/mobile.png` are present only when
  Node, Playwright, and a launchable Chromium/Chrome browser are available.

The default fixture is `examples/dogfood-dashboard-review-archive.json`. It has
fixed IDs, timestamps, tags, linked PRs, passing evidence, one failing evidence
item, and synthetic redaction-sensitive output. To exercise live CLI seeding
instead of deterministic import:

```bash
scripts/dashboard-review-dogfood.sh --seed-live
```

To keep screenshots out of a docs-only or shell-only check:

```bash
scripts/dashboard-review-dogfood.sh --skip-screenshots
```

## Reviewer Checks

Open `dashboard/index.html` directly in a browser. The export is static local
HTML with no JavaScript or remote assets.

Inspect these populated areas:

- top metrics: records, evidence items, PR links, and failed evidence;
- recent records: tags, synthetic workspaces, and linked PR URLs;
- evidence previews: passing output, warning output, and the failed visual check;
- tags: repeated `dogfood` and `dashboard` counts;
- redaction/privacy copy: confirms the file must be reviewed before sharing.

Some sections are expected to be sparse in this CLI dogfood path. The CLI
`ctx dashboard export` currently renders records and evidence from the local
store; richer internal metadata sections are covered by report-library fixtures.
When reviewing this script's dashboard, sparse messages are expected for:

- sessions and runs;
- timeline;
- transcript, messages, and tool calls;
- evidence status;
- files touched;
- Git and jj state;
- artifact cards.

Treat those sparse sections as layout and empty-state review targets. They
should be readable, non-overlapping, and clear about why the section has no
data. Do not treat sparse sections in this dogfood output as missing CLI data
unless the script or manifest reports a blocker.
