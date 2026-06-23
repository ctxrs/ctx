# ctx Dashboard

Local React/Vite shell used for two local preview surfaces:

- the static dashboard exported by `ctx dashboard export`;
- a docs/site preview at `site-preview.html` for ctx positioning and
  release-copy review.

- React renders a static, local-only SPA from `#ctx-dashboard-data`.
- Rust owns the share-safe normalized DTO and embeds it into `dist/index.html`.
- The UI uses Tailwind styles, Radix tabs, TanStack Table, and Recharts without importing ADE runtime state.
- Provider views render the share-safe session/event/run/artifact DTOs already exported by Rust. Sparse provider states should explain whether the export has no work yet or whether the provider path only has summary-level fidelity.
- Playwright screenshots are written to `target/ctx-artifacts/dashboard-react`.

Useful commands:

```bash
npm install
npm run build
npm run test
```

After `npm run build`, preview outputs are written to `dist/`:

- `dist/index.html` is the dashboard shell used by export work.
- `dist/site-preview.html` is the local docs/site preview for the `0.1.0`
  candidate wording pass.

Refresh story:

```bash
ctx dashboard export --output ./ctx-dashboard
```

The dashboard is a static export. Re-run the export command after new
`ctx capture import`, provider fixture import, evidence, or PR-link activity.
Opening an old `index.html` never refreshes the local SQLite store by itself.
