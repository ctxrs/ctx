# Visual Regression Surfaces

This is the first-pass capture inventory for the shared web UI and Tauri shell.

Goals:

- Catch unapproved layout and chrome changes from architecture work, not just feature work.
- Keep the suite small enough that agents can iterate quickly and humans can review diffs without noise.
- Snapshot seeded, deterministic states instead of broad smoke flows.

Capture defaults:

- Themes:
  - `dark` for all core workbench states
  - `light` and `dark` for settings and updater surfaces
- Widths:
  - `1400x900` for standard desktop workbench states
  - `900x900` for tighter split and sidebar pressure states
  - `1600x900` for diff-pane expansion
  - `760x900` for narrow diff-pane and responsive stress
  - `1280x900` for full-page setup and settings flows
- Stability:
  - use seeded fixtures, fixed locale, fixed theme, fixed time when possible
  - disable or mask dynamic surfaces such as clocks, progress timers, and status bars
  - prefer one named state per visual contract rather than long end-to-end journeys

Priority 1:

- Session thread and composer stack
  - widths: `1400x900`, `900x900`
  - themes: `dark`
  - states:
    - idle thread with a long transcript
    - active run with streaming/tool activity
    - queued follow-up messages visible
    - auth-required banner
    - provider-guard warning
    - load-issues or fatal-state banner
- Workbench shell and task list
  - widths: `1400x900`, `1000x900`
  - themes: `dark`
  - states:
    - empty/new-task state
    - mixed row states with unread, running, and error items
    - archived section expanded with a populated list
- Diff pane
  - widths: `1600x900`, `760x900`
  - themes: `dark`
  - states:
    - healthy diff with long paths and multi-file content
    - fetch failure state
    - unavailable `no_repo` state
    - too-large diff state
- Settings
  - widths: `1280x900`
  - themes: `light`, `dark`
  - states:
    - general settings shell
    - harness authentication section
    - harness install/update progress or failure state
    - resource utilization populated state
- Update notice
  - widths: `1280x900`
  - themes: `light`, `dark`
  - states:
    - passive update banner
    - update info modal
    - forced-update overlay

Priority 2:

- Diagnostics page
  - width: `1280x900`
  - themes: `dark`
  - states:
    - populated diagnostics with missing components and update metadata
- Workspace setup wizard
  - widths: `1280x900`, `960x900`
  - themes: `dark`
  - states:
    - workspace location step
    - provisioning/download progress and failure states
    - advanced titling step
    - launch-log side panel
    - final confirmation summary

Suggested first wave:

- Start with 10 to 14 snapshots across the five Priority 1 areas.
- Keep shell-specific chrome separate from shared web UI content so desktop-only changes do not churn all baselines.
- Add new baselines only for durable product surfaces, not transitional implementation details.

Suggested naming:

- `session-idle-dark-desktop`
- `session-running-dark-narrow`
- `task-list-mixed-dark-desktop`
- `diff-pane-too-large-dark-wide`
- `settings-general-light`
- `settings-harness-auth-dark`
- `update-forced-light`
