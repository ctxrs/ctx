# Updater Global Semantics Contract

## Scope
This contract defines deterministic updater semantics for desktop-mode UI across launcher (`/`), workspace setup (`/workspace-setup`), and workbench (`/workspaces/:id`) routes, including multi-window behavior.

## Truth Model
- Source of truth for desktop updater phase is native backend state from `desktop_get_app_update_state`.
- Source of truth for policy metadata is `/api/updates/check` (channel, minimum version, platform support).
- Browser storage keys (`ctx_update_prompt_next_allowed_at_v1`, `ctx_update_prompt_idle_versions_v1`) are intent/preferences only.
- Browser storage must never override native truth for `available`, `staged`, `restart_required`, `phase`, or `latest_version`.

## State Contract
| Native state | Required fields | Banner visibility | Primary action | Secondary action |
| --- | --- | --- | --- | --- |
| `configured=false` | `configured=false`, `message` explains reason | Hidden | None | None |
| `phase=staging` | `latest_version` present, `restart_required=false` | Hidden on desktop | None | None |
| `phase=staged_ready` | `staged=true`, `restart_required=false` | Hidden on desktop while native apply runs | None | None |
| `restart_required=true` + `phase=restart_required` | `latest_version` present | Visible on all routes/windows | `Relaunch` triggers `desktop_restart_app` | `Update on Next Idle` schedules restart |
| `phase=failed` or `last_error` | `last_error` or `message` set | Visible only when restart-required banner is visible | `Relaunch` remains available in restart-required | Secondary action remains available unless applying |

## Route Invariants
- Updater checks are app-scoped, not page-scoped.
- Navigation between launcher, workspace setup, and workbench must not reset updater truth.
- Manual refresh (`ctx:request-update-check`) must trigger a native state refresh regardless of route.

## Multi-Window Invariants
- Restart-required prompt must converge on every open desktop window for the same workspace context.
- Scheduling `Update on Next Idle` in one window must become visible to all windows via shared intent state and refresh signaling.
- Active-task state must block idle restart; idle transition must trigger restart requests.

## Diagnostics Requirements
- E2E harness must record:
  - native updater state snapshots over time,
  - command call order/count (`desktop_get_app_update_state`, `desktop_restart_app`, `desktop_apply_app_update`),
  - failure messages for restart/apply paths.

## Validation Targets
- `core/apps/web/e2e/updater-global-semantics.spec.ts`
- `core/apps/web/e2e/updater-desktop-ui-contract.spec.ts`
