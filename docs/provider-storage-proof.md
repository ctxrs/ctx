# Provider Storage Proof Notes

This file records public proof notes for provider-owned local history formats
that the shipped CLI supports. It is intentionally narrower than the full
research trail: if a provider is not listed in the support matrix, ctx does not
claim local-history ingestion for it on this branch.

The support contract lives in
[`provider-support-matrix.json`](provider-support-matrix.json).

## Kiro CLI

- Official Kiro CLI documentation says chat sessions are auto-saved after each
  turn and keyed by directory path.
- Running the official Kiro CLI 2.10.0 Linux binary with temporary home/config
  directories created `$XDG_DATA_HOME/kiro-cli/data.sqlite3`.
- The generated SQLite DB included `conversations_v2`, where records store JSON
  values containing conversation history.
- ctx imports the bounded SQLite shape as `kiro_cli_sqlite`.

## CodeBuddy

- WayLog's CodeBuddy reader documents local storage roots and message layout for
  CodeBuddy extension history.
- Project folders are keyed by project path hash, `index.json` files describe
  conversations/messages, and `messages/<id>.json` stores each message.
- ctx imports this bounded local file tree as `codebuddy_history_json`.

## CodeArts Agent

- Huawei CodeArts Agent VSIX evidence shows local SQLite storage under the
  extension global storage area.
- The supported importer reads the kernel database shape only; legacy JSON cache
  files are not part of the claim.
- ctx imports this bounded SQLite shape as `codearts_agent_kernel_sqlite`.

## Zencoder

- Zencoder local chat-session evidence comes from extension constants and the
  public `opik-chat-history` exporter.
- The supported importer reads `zencoder-chat/sessions.json` plus per-session
  JSON files when present.
- ctx imports this bounded local file tree as `zencoder_chat_sessions_json_tree`.

## Trae

- Trae and Trae CN persist workspace state in VS Code/Electron-style
  `state.vscdb` files under user workspace-storage roots.
- The importer reads only recognized chat-history keys from those databases.
- `ctx import --provider trae-cn` is accepted as an alias and stores rows under
  the canonical `trae` provider.
- ctx imports this bounded SQLite shape as `trae_state_vscdb`.

## AstrBot

- AstrBot stores bounded local application state in `data_v4.db`.
- The importer reads local LLM context plus available platform history rows when
  present.
- ctx imports this bounded SQLite shape as `astrbot_data_v4_sqlite`.

## Warp

- Warp local restoration evidence shows agent task content in provider-owned
  `warp.sqlite` databases.
- ctx records the presence of server token fields only as booleans and does not
  copy those token values into metadata.
- Cloud sync, browser IndexedDB, command history outside agent tasks, Warp Drive,
  and team data are not part of this claim.
- ctx imports this bounded SQLite shape as `warp_sqlite`.
