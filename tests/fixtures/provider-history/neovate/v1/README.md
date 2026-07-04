# Neovate native JSONL fixture

This fixture is a sanitized Neovate project-history tree for `@neovate/code@0.28.5` (`neovateai/neovate-code`).

Source anchors from the packed npm package:

- `dist/index.d.ts` defines `NormalizedMessage` as `Message & { type: "message"; timestamp; uuid; parentUuid; metadata? }`.
- The same type declarations define `Paths` with `globalConfigDir`, `globalProjectDir`, `projectConfigDir`, `fileHistoryDir`, and `getSessionLogPath`.
- The bundled `index.mjs` `Paths` implementation stores global projects under `~/.neovate/projects/<sanitized-cwd>`, returns `<globalProjectDir>/<sessionId>.jsonl` for normal session IDs, and keeps file history in `<globalProjectDir>/file-history`.
- The bundled `JsonlLogger` appends normalized `type:"message"` rows and snapshot rows; `RequestLogger` writes request logs under `<globalProjectDir>/requests/<requestId>.jsonl`.

The fixture uses `/workspace/neovate` sanitized to `-workspace-neovate`. The `requests/` and `file-history/` siblings are intentionally present to prove they are ignored by the session importer.
