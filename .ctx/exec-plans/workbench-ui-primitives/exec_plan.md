# Workbench UI Primitives And Templates

## Objective

Make Workbench UI composition a first-class architecture in the ADE. Centralize
the existing workbench around typed UI primitives, then ship built-in templates
that prove the model:

- Classic Workbench: existing default behavior preserved.
- Kanban Work Board: task cards grouped into lanes.
- Multipane Workbench: VS Code-like resizable panes.
- Review Console: focused review/evidence surface.

Do all work locally on the current `ctx/agent-work-semantics` branch. Do not
push.

## End Conditions

Implementation is complete only when:

- The docs memo has a concrete UI primitive/template decision section.
- The web app has a typed primitive/template model, not one-off pages.
- Existing classic workbench behavior remains the default.
- Users can switch built-in templates from the workbench UI.
- Template choice persists per workspace/window through existing UI persistence.
- Kanban, multipane, and review templates render from the same primitive set.
- Multipane template has real resizable split panes with persisted sizes.
- The templates reuse existing Workbench state/actions; they do not duplicate
  session/task/diff/artifact/terminal business logic.
- Tests cover primitive registry/model behavior, persistence decode/encode,
  template switching, kanban grouping, multipane resizing state, and at least
  one render smoke per built-in template.
- Frontend typecheck, lint, full web tests, Rust workspace tests affected by
  route/type changes, formatting, and diff hygiene pass.
- A reviewer subagent reviews final architecture and implementation; all
  actionable findings are fixed or explicitly recorded.

## Implementation Boundaries

Keep this first slice frontend-centered. Do not build hosted sync, arbitrary
third-party iframe UIs, a visual drag/drop layout editor, or plugin package
installation for templates in this slice.

The plugin-facing contract should be visible in types/docs, but built-in
templates are the proof implementation.

## Proposed File Areas

- `docs/work-ade-extension-decisions.mdx`
- `core/apps/web/src/workbench/types.ts`
- `core/apps/web/src/workbench/persistence.ts`
- `core/apps/web/src/workbench/store.tsx`
- `core/apps/web/src/pages/workbenchShell/WorkbenchPage.shell.tsx`
- `core/apps/web/src/pages/workbenchShell/WorkbenchPageShellView.tsx`
- new `core/apps/web/src/pages/workbenchShell/workbenchUiPrimitives*`
- new `core/apps/web/src/pages/workbenchShell/workbenchTemplates*`
- `core/apps/web/src/styles/workbench.css`
- focused tests under `core/apps/web/src/pages/workbenchShell/` and
  `core/apps/web/src/workbench/`

## Validation Plan

Run, at minimum:

```bash
pnpm --dir core/apps/web typecheck
pnpm --dir core/apps/web lint
pnpm --dir core/apps/web test
cargo fmt --manifest-path core/Cargo.toml --all -- --check
cargo test --manifest-path core/Cargo.toml --workspace --locked
git diff --check
```

If no Rust files are touched after final implementation, still run at least the
Rust workspace test once because this branch already carries backend Work graph
changes from the prior session.
