# docs-site

The public source for the prerendered docs site at `ctx.rs`. Ordinary builds
and tests use this checkout's `docs-content/` and `services/docs-site/` only.

## Commands

From this directory:

- `pnpm install --frozen-lockfile`
- `pnpm dev`
- `pnpm build`
- `pnpm preview`
- `pnpm test`

The normal Bazel test owner is `//services/docs-site:docs_site_test`, using the
shared package-action runner and declared npm dependencies. From the repository
root, run `scripts/bazelw test //services/docs-site:docs_site_test`. The test
materializes writable inputs in its temporary directory; it does not generate
files in the source checkout or require a private repository.

`pnpm build` generates page data, search, Markdown exports, discovery assets,
prerendered HTML, `_redirects`, and `_headers`. Generated files stay ignored;
regenerate them with the owning command instead of editing them.

## Routes

`/blame` is the canonical Blame guide. Former `/pro` and `/pro/index` routes
redirect there; their Markdown exports remain Markdown. Former referral URLs
lead to legacy support and legal information. `/cloud` still aliases `/teams`,
`/slack` keeps its configured destination, and shared fonts retain cross-origin
access. Product aliases apply only to content roots containing the Blame guide.

## Optional ADE archive

The archive content is supplied separately by its operator. It is not needed
for a normal ctx build. The existing `CTX_DOCS_CONTENT_ROOT` configuration
accepts an absolute content directory containing `docs.json` and its pages and
assets. Explicitly provide that directory when invoking the archive script:

```bash
CTX_DOCS_CONTENT_ROOT=/absolute/path/to/archive/docs-content pnpm build:ade-archive
```

This script fails if the root is missing or empty. It sets the existing
`VITE_CTX_SITE_ORIGIN=https://ade.ctx.rs` and archive installer command while
using the same renderer. `pnpm deploy:ade-archive` consumes the same explicit
root and deploys to the `ctx-ade-site` Cloudflare Pages project. There is no
private-checkout fallback or built-in archive path.

## Deployment

`pnpm deploy` explicitly builds and deploys to the `ctx-site` Cloudflare Pages
project. `wrangler.toml` points to `dist/`. Keep production deployment manual;
do not connect Pages to automatic deployment from `main`. Review the built
source candidate before an authorized deployment.

Visitor analytics use the existing Cloudflare integration. No signing or
service credentials belong in this source tree.
