# ctx Dashboard Dogfood

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
- provider import JSON files are present for live-seeded Codex, Claude, and Pi
  fixtures when `--seed-live` is used.
- `manifest.json` lists artifact-relative paths and screenshot status. It
  intentionally omits raw absolute data-root, repository, home, and browser
  scratch paths so the default artifact set can be attached for review after
  checking its contents.
- `screenshots/desktop.png` and `screenshots/mobile.png` are present only when
  Node, Playwright, and a launchable Chromium/Chrome browser are available.

The default artifact set does not include `ctx export` output. Raw archives are
portable/private data and may contain raw command or artifact content; generate
one explicitly only when reviewing private local data.
The script fails if generated default artifacts contain the repository root,
`$HOME`, the raw `CTX_DATA_ROOT`, browser profile/cache scratch paths, or known
synthetic secret markers.

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

Open `dashboard/index.html` directly in a browser. The export is a local
React/Vite dashboard with bundled local JavaScript and no remote assets.

Inspect these populated areas:

- top metrics: records, evidence items, PR links, and failed evidence;
- recent records: tags, synthetic workspaces, and linked PR URLs;
- provider/session detail: provider cards, selected session metadata, prompt
  previews, assistant/tool events, redaction badges, and sparse-fidelity copy;
- evidence previews: passing output, warning output, and the failed visual check;
- tags: repeated `dogfood` and `dashboard` counts;
- redaction/privacy copy: confirms the file must be reviewed before sharing.

Some sections are expected to be sparse in this CLI dogfood path. With
`--seed-live`, provider fixture imports populate sessions, timeline, and
provider event, message, and tool-call fixture views. Sparse messages are still
expected for:

- files touched;
- Git and jj state;
- artifact cards when no share-safe artifact preview is available.

Treat those sparse sections as layout and empty-state review targets. They
should be readable, non-overlapping, and clear about why the section has no
data. Do not treat sparse sections in this dogfood output as missing CLI data
unless the script or manifest reports a blocker.

Refresh the dashboard by re-running the dogfood script or `ctx dashboard export`
after new local capture/import activity; the exported `index.html` is static and
does not poll the local database.
