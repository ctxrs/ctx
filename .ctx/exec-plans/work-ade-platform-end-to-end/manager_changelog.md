# Manager Changelog

Record each local commit or integrated worker handoff here.

## Entries

- `d3e24fe` - Plan Work ADE platform completion.
  - Added detailed end-to-end execution plan, completion records, and public
    Work/ADE extension decision docs.
  - Reviewed by Ohm and Locke before commit.
- `5dc809d` - Add composable workbench templates.
  - Added Classic/Kanban/Multipane/Review template infrastructure, persisted
    template state, Workbench split panes, Work task board/detail projection,
    and focused frontend tests.
  - Focused Vitest template/projection suite passed after commit.
- `729d953` - Harden Work artifact paths.
  - Added safe relative path validation for Work artifact/file endpoints in
    schema and store tests.
  - Focused `ctx-store agent_work` tests passed after commit.
- `399b29e` - Harden plugin inventory runtime.
  - Marked duplicate plugin IDs as load errors, preserved command stdin
    BrokenPipe handling, and hardened a relative command test.
  - Focused duplicate-plugin daemon test passed after commit.
- `6a36194` - Add safe local validation wrappers.
  - Added `cargo-safe`, `check-local`, and Makefile `check`/safe `test` target.
  - Shell syntax check passed after commit.
