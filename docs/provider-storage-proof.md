# Provider Storage Proof Notes

These notes capture providers that were researched while adding native
IDE/application storage imports.

## OpenHands

- Source evidence: OpenHands `get_default_persistence_dir()` checks
  `OH_PERSISTENCE_DIR`, then legacy `FILE_STORE_PATH`, then `~/.openhands`.
- The filesystem event service stores events as JSON with
  `event.model_dump_json(indent=2)`.
- Conversation paths are
  `<persistence>/<user_id>/v1_conversations/<conversation-id-hex>/<event-id-hex>.json`.
- `ctx` imports this shape as `openhands_file_events` without OpenHands runtime
  dependencies.

## Zed

- Source evidence: Zed persists agent threads in SQLite at
  `<Zed data dir>/threads/threads.db`.
- Table shape observed in source:
  `threads(id, summary, updated_at, data_type, data, parent_id, folder_paths, folder_paths_order, created_at)`.
- The `data` column is zstd-compressed JSON for `DbThread`, including messages,
  summaries, token usage, model/profile metadata, and draft prompt state.
- `ZED_STATELESS` disables persistent DB use.
- Gap: native import needs a zstd decoder dependency plus a stable fixture for
  compressed `DbThread` JSON. This pass did not add that dependency.

## Void

- Source evidence: Void stores chat threads through VS Code/Electron application
  storage.
- Current key: `void.chatThreadStorageII`; older keys:
  `void.chatThreadStorage` and `void.chatThreadStorageI`.
- Values are JSON-serialized chat thread/message structures in application
  storage.
- Gap: native import needs a small, proven reader for the VS Code/Electron
  storage location and key formats, plus fixtures covering all three keys.
