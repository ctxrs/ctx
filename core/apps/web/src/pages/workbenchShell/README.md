# workbenchShell

Boundary modules for `WorkbenchPage.shell.tsx`.

- `useWorkbenchOptimisticTasks.ts` and `useWorkbenchTaskCreation.ts` together implement the optimistic task lifecycle for instant "New Task" UX.
- `optimisticStartingTaskRef` is intentional product behavior, not an accidental fallback. It exists to bridge the brief gap between immediate focus/navigation and committed optimistic task state.
- Lifecycle contract:
  - `useWorkbenchTaskCreation.ts` creates the optimistic task/session/message shell, focuses it immediately, and seeds `optimisticStartingTaskRef` during the same `flushSync`.
  - `useWorkbenchOptimisticTasks.ts` projects optimistic task state from React state first, but will use `optimisticStartingTaskRef` for the active task on the first paint if focus lands before the optimistic summary is committed.
  - Once the optimistic task is present in `optimisticTasksById`, or focus moves to another task, `optimisticStartingTaskRef` must be cleared.
  - Shell consumers must treat the optimistic session id as transient until the optimistic task is committed, so session open/diff side effects do not run against it too early.
  - Success path: optimistic task reconciles to `localStatus: "synced"` with stable client ids.
  - Failure path: optimistic task remains visible with `localStatus: "failed"` and failure metadata until dismissed.

- `useWorkbenchProviders.ts`: provider install/poll/options orchestration.
- `useWorkbenchOptimisticTasks.ts`: optimistic task/session state projection.
- `workbenchTaskActivity.ts`: pure task activity/session selection derivations.
- `useWorkbenchSessionBridge.ts`: workbench-owned bridge between shell focus, workspace snapshots, and the session supervisor.
- `useWorkbenchTaskListController.tsx`: sidebar/task-list controller for archive, rename, menus, virtualization, and optimistic task dismissal.
- `useWorkbenchActiveTaskController.ts`: active-task controller for diff/artifacts/session panes, terminal state, worktree lookup, session actions, and conversation menu state.
- `useWorkbenchTaskCreation.ts`: optimistic task/session/message bootstrap and reconciliation.
- `useWorkbenchChromeIntegration.ts`: desktop menu, titlebar, and local chrome integration side effects.
- `useWorkbenchTaskActivity.ts`: compatibility shim that re-exports the split task-activity bridge/helpers.
- `useWorkbenchTaskScrollbar.ts`: task list scroller lifecycle handlers.
- `useWorkbenchDesktopMenu.ts`: desktop menu command/state wiring for workbench-scoped actions.
- `useWorkbenchDiffPane.ts`: diff summary normalization/guards.
- `useWorkbenchDragDropAttachments.ts`: composer drag/drop attachment ingestion.
- `WorkbenchSessionHeader.tsx`: single-session header presentation component.
- `WorkbenchEmptyState.tsx`: blank-workbench composer/onboarding composition for the no-active-task shell state.
