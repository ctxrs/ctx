# Providers

ctx imports existing agent history through provider adapters. Each adapter must
make a narrow, testable claim about the source format it reads and the event
fields it indexes.

## Supported Local Imports

The current CLI imports local history for:

- Codex session JSONL trees under `~/.codex/sessions`;
- Codex `~/.codex/history.jsonl`;
- Pi session JSONL files under `~/.pi/agent/sessions` or `~/.omp/agent/sessions`
  (Oh My Pi fork);
- Claude Code project JSONL transcripts under `~/.claude/projects`;
- OpenCode SQLite history under `~/.local/share/opencode/opencode.db`;
- Kilo Code SQLite history from `KILO_DB`, `$XDG_DATA_HOME/kilo/kilo.db`,
  `~/.local/share/kilo/kilo.db`, or Kilo channel DBs named `kilo-*.db`;
- Crush SQLite history from `CRUSH_GLOBAL_DATA/crush.db`,
  `$XDG_DATA_HOME/crush/crush.db`, `~/.local/share/crush/crush.db`,
  configured `data_directory` values, project `.crush/crush.db`, or an
  explicit Crush DB path;
- Goose sessions SQLite history from `GOOSE_PATH_ROOT/data/sessions/sessions.db`,
  `$XDG_DATA_HOME/goose/sessions/sessions.db`,
  `$XDG_DATA_HOME/Block/goose/sessions/sessions.db`, matching defaults under
  `~/.local/share`, or an explicit Goose sessions DB path;
- Dexto SQLite history from `~/.dexto/database/*.db`, current project
  `.dexto/database/*.db`, or an explicit Dexto DB path;
- Lingma SQLite history from
  `~/.lingma/vscode/sharedClientCache/cache/db/local.db`,
  `~/.lingma/vscode-insiders/sharedClientCache/cache/db/local.db`, or an
  explicit Lingma `local.db` path. Lingma assistant content is imported from
  `summary`/`error_result` only and may be partial;
- Qoder transcript JSONL files under
  `~/.qoder/projects/<project>/transcript/<session-id>.jsonl`,
  `~/.qoder/projects`, or an explicit transcript file/directory. This importer
  reads the official transcript format and does not parse encrypted Qoder app
  logs or VS Code/Electron state databases;
- Windsurf official Cascade hook transcript JSONL files under
  `~/.windsurf/transcripts`, or an explicit transcript file/directory. Hook
  transcripts are usually opt-in and capture sessions after hook setup; ctx
  does not parse `~/.codeium/windsurf/cascade` or guessed VS Code state
  databases;
- Warp Terminal local restoration SQLite history from documented Linux, macOS,
  and Windows `warp.sqlite` paths or an explicit `warp.sqlite` path. Discovered
  local restoration DBs are native auto-importable; cloud sync endpoints,
  Oz/cloud conversations, browser IndexedDB, Markdown exports, command history
  outside `agent_tasks`, and Warp Drive/team data are not parsed;
- CodeBuddy JSON history under `~/.codebuddy`,
  `~/Library/Application Support/CodeBuddyExtension/Data`,
  `%LOCALAPPDATA%/CodeBuddyExtension`, or an explicit CodeBuddy history root;
- CodeArts Agent kernel SQLite rows for `opencode.db` under
  `~/.codeartsdoer/vscode-data`, `~/.codeartsdoer/codearts-data`,
  XDG data homes, or an explicit DB path. This importer is limited to the
  kernel-managed OpenCode-derived SQLite DB and does not parse older/private VS
  Code cache JSON;
- Zencoder chat session trees under common VS Code-family app-data
  `User/globalStorage/ZencoderAI.zencoder/zencoder-chat` roots, or an explicit
  `zencoder-chat` tree/session path. Imports are limited to `sessions.json` and
  `sessions/*.json`; `.zencoder` skill/config homes and other extension caches
  are not parsed;
- Syncfusion Code Studio session-store SQLite DBs under Code Studio app-data
  `User/globalStorage/session-store.db`, or an explicit session DB path. This
  importer is limited to the proven session-store DB and does not parse
  `.codestudio` skills/agents/settings or debug logs;
- Aider Desk project task context files under `.aider-desk/tasks/<taskId>`,
  `AIDER_DESK_DIR/tasks/<taskId>`, or an explicit task, tasks, context file, or
  project root;
- Trae chat state from Trae and Trae CN `User/workspaceStorage` roots at
  `~/Library/Application Support/Trae/User/workspaceStorage`,
  `~/Library/Application Support/Trae CN/User/workspaceStorage`,
  `%APPDATA%/Trae/User/workspaceStorage`, or
  `%APPDATA%/Trae CN/User/workspaceStorage`, plus explicit
  `User/workspaceStorage` roots, workspace directories, or `state.vscdb` files.
  The importer reads known VS Code-style `ItemTable` keys only; `trae-cn` is an
  alias for canonical provider `trae`. CN input-history rows are often user
  prompts only, and ctx does not claim `globalStorage`, `ModularData`, arbitrary
  caches, or unknown `ItemTable` keys;
- OpenClaw session JSONL trees under `OPENCLAW_STATE_DIR`, `~/.openclaw`,
  legacy `~/.clawdbot`, or legacy `~/.moltbot`;
- Hermes Agent SQLite history under `HERMES_HOME/state.db` or
  `~/.hermes/state.db`;
- NanoClaw project history from a project root with `data/v2.db` and
  `data/v2-sessions` when imported explicitly;
- AstrBot local SQLite history from `ASTRBOT_ROOT/data/data_v4.db`,
  `~/.astrbot/data/data_v4.db`, or a project `data/data_v4.db`;
- Shelley SQLite history from `SHELLEY_DB`, `~/.config/shelley/shelley.db`, or
  an explicit Shelley DB path;
- Terramind/Nucleus SQLite history from `$XDG_CONFIG_HOME/Nucleus/data/agents.db`,
  `~/.config/Nucleus/data/agents.db`, platform app-data equivalents, or an
  explicit `agents.db`;
- Continue CLI sessions from `CONTINUE_GLOBAL_DIR/sessions`,
  `~/.continue/sessions`, or an explicit Continue sessions path;
- OpenHands event JSON under `OH_PERSISTENCE_DIR`, legacy `FILE_STORE_PATH`,
  `~/.openhands`, or an explicit persistence root;
- Antigravity transcript JSONL mirrors under
  `~/.gemini/antigravity-cli/brain/*/.system_generated/logs/transcript_full.jsonl`
  or `transcript.jsonl`;
- Gemini CLI chat JSONL records under `~/.gemini/tmp/**/chats/**/*.jsonl`;
- Tabnine CLI chat JSONL records under
  `~/.tabnine/agent/tmp/**/chats/**/*.jsonl`;
- Cursor CLI agent transcript JSONL files under
  `~/.cursor/projects/**/agent-transcripts/**/*.jsonl`;
- Zed agent thread SQLite DBs at `$XDG_DATA_HOME/zed/threads/threads.db` or
  `~/.local/share/zed/threads/threads.db`;
- Copilot CLI session event logs named `events.jsonl` under
  `~/.copilot/session-state`;
- Factory AI Droid session JSONL files under `~/.factory/sessions`;
- Qwen Code chat JSONL files under `QWEN_RUNTIME_DIR/projects`,
  `QWEN_HOME/projects`, or `~/.qwen/projects`;
- Kimi Code CLI wire JSONL records under `KIMI_CODE_HOME` or `~/.kimi-code`
  session trees;
- Autohand Code session JSONL records under `AUTOHAND_HOME/sessions` or
  `~/.autohand/sessions`, where each session has `metadata.json` and
  `conversation.jsonl`;
- iFlow CLI session JSONL transcripts under `IFLOW_HOME/projects` or
  `~/.iflow/projects`, where project directories contain `session-*.jsonl`;
- Eve local Workflow `.workflow-data` streams from `WORKFLOW_LOCAL_DATA_DIR`,
  a current project `.workflow-data`, or an explicit `.workflow-data`/project
  path;
- Junie session event streams from `JUNIE_SESSIONS_DIR`,
  `JUNIE_HOME/sessions`, `~/.junie/sessions`, or an explicit sessions/session
  path containing `index.jsonl` and `session-*/events.jsonl`;
- TinyCloud project session JSONL files under
  `$TINYCLOUD_HOME/projects/*/sessions/*.jsonl`,
  `~/.tinycloud/projects/*/sessions/*.jsonl`, legacy `<home>/sessions/*.jsonl`,
  or an explicit TinyCloud home/session path;
- ForgeCode conversation SQLite history from `FORGE_CONFIG/.forge.db`, legacy
  `~/forge/.forge.db`, `~/.forge/.forge.db`, or an explicit ForgeCode DB path;
- Deep Agents LangGraph checkpoint SQLite history from
  `~/.deepagents/.state/sessions.db` or an explicit Deep Agents `sessions.db`
  path;
- Mistral Vibe session directories under `VIBE_HOME/logs/session` or
  `~/.vibe/logs/session`, where each session has `meta.json` and
  `messages.jsonl`;
- Mux session transcripts under `MUX_ROOT/sessions` or `~/.mux/sessions`,
  where each workspace directory has `chat.jsonl` and optional `partial.json`
  plus archived subagent transcripts;
- Moxby chat SQLite history under `MOXBY_STATE_DIR`,
  `$XDG_DATA_HOME/com.moxby.agent/moxby_chats.db`, common app-data
  `com.moxby.agent/moxby_chats.db` paths, or an explicit Moxby state
  directory/DB path;
- Reasonix session JSONL files under `~/.reasonix/sessions`, including
  adjacent `.events.jsonl`, `.meta.json`, `.pending.json`, and `.plan.json`
  sidecars;
- AdaL event-sourced JSONL sessions under `~/.adal/sessions`, where each
  session is named `conversation_<id>.jsonl` and may have a sibling
  `<id>_metadata.json` sidecar;
- Kode project JSONL transcripts under `KODE_CONFIG_DIR/projects`,
  `CLAUDE_CONFIG_DIR/projects`, or `~/.kode/projects`;
- Neovate project session JSONL transcripts under `~/.neovate/projects`,
  excluding request and file-history sidecars;
- Cline task JSON directories under `CLINE_DATA_DIR`, `CLINE_DIR/data`,
  `~/.cline/data`, or common VS Code globalStorage folders;
- Roo Code task JSON directories under `roo-cline.customStoragePath`, common
  VS Code globalStorage folders for `RooVeterinaryInc.roo-cline`, or an
  explicit path.
- IBM Bob IDE task JSON directories under app-data folders named `IBM Bob` or
  `Bob-IDE`, specifically `User/globalStorage/ibm.bob-code/tasks`.

These are built-in provider adapters for native local history. The custom
history format is separate: `ctx import --format ctx-history-jsonl-v1 --path
<file>` reads an explicit JSONL interchange file from any exporter, and
history-source plugins can stream the same format from local adapter commands.
Custom history is stored internally under the bounded provider `custom` while
preserving the exporter's `provider_key`, `source_id`, and `session_id` as
metadata and ID namespace components. File imports are not auto-discovered;
local plugin manifests are listed by `ctx sources`.

For the pinned npx skills target set, see
[`agent-storage-coverage.md`](agent-storage-coverage.md). That ledger maps each
npx agent id to native ctx import support, reusable storage family, and current
gap.

Use `ctx sources` for the truth on the current machine:

```bash
ctx sources
ctx sources --json
```

CLI provider flags use names such as `kilo`, `crush`, `goose`, `dexto`,
`openclaw`, `hermes`,
`nanoclaw`, `astrbot`, `shelley`, `continue`, `openhands`, `copilot-cli`,
`factory-ai-droid`, `qwen-code`, `kimi-code-cli`, `autohand-code`,
`kiro-cli`, `iflow-cli`, `eve`, `codearts-agent`, `forgecode`, `deepagents`, `mistral-vibe`, `mux`,
`reasonix`, `adal`, `kode`, `neovate`, `terramind`, `zed`, `lingma`, `qoder`, `pochi`,
`warp`, `codebuddy`, `aider-desk`, `trae`, `tinycloud`, `windsurf`, `cline`, and `roo`/`roo-code`.
Structured JSON and stable SQL views use provider IDs in ctx output; multiword IDs may be
snake_case, such as `copilot_cli`, `factory_ai_droid`, `qwen_code`,
`kimi_code_cli`, `autohand_code`, `kiro_cli`, `iflow_cli`, or
`mistral_vibe`; CodeArts Agent is reported as `codearts_agent`, Aider Desk is
reported as `aider_desk`, while compact native
IDs such as `kilo`, `openclaw`, `crush`, `goose`, `dexto`, `mux`, `reasonix`,
`adal`, `kode`, `neovate`, `terramind`, `zed`, `lingma`, `qoder`, `pochi`, `codebuddy`,
`forgecode`, `deepagents`, `nanoclaw`, `astrbot`, `trae`, `tinycloud`, `windsurf`, `warp`,
`shelley`, `continue`, and `openhands`
stay compact. Roo Code is
reported as `roo_code`.

`ctx sources --json` reports each known provider source with `import_support`
and `importable` fields. A native source is marked available/importable only
when provider-specific transcript files exist. Sources with
`import_support: "preview"` are explicit-import preview paths: use
`ctx import --provider nanoclaw` when discovery finds the desired source, or use
`ctx import --provider trae --path <state.vscdb-or-workspaceStorage>` for Trae.
Add `--path` to target a specific source before searching it. Explicit and
preview paths are intentionally excluded from `ctx import --all` and
pre-search refresh until
they have safe default discovery. Sources with
`status: "unknown"` hit the bounded transcript probe budget before proving
history exists, and sources with `import_support: "unsupported"` are detections
or blockers, not importable native history.

If a provider is selected without a proven native importer, `ctx import`
returns a provider-specific native-history blocker. Do not document a provider
as natively locally importable until the CLI can discover or parse that
provider's real local history and the provider support matrix marks the shipped
path accordingly.

## Provider Smoke

Public provider smoke coverage uses static local-history fixtures. It verifies
supported imports, unsupported-provider blockers, provider filtering, citations,
and deterministic search without executing provider CLIs, reading real user
history, requiring API keys, or making network calls:

```bash
bazel test //:provider_fixture_e2e --config=ci
```

## Import Rules

Provider imports should be:

- read-only with respect to provider-owned files;
- explicit through `ctx import`;
- safe to interrupt and re-run, using idempotent rescans or provider cursors
  when available;
- idempotent for unchanged source files;
- clear about which fields were indexed and which were left raw-only;
- conservative when a transcript schema is unknown or malformed.

Custom history imports follow the same read-only and idempotent principles, but
their compatibility contract is the `ctx-history-jsonl-v1` schema rather than a
provider-owned native transcript format.

## Cline, Roo Code, And IBM Bob Notes

Cline, Roo Code, and IBM Bob import support reads file-backed task directories,
not VS Code's private extension state databases. The importer looks for task
folders containing files such as `api_conversation_history.json`,
`ui_messages.json`, `task_metadata.json`, `history_item.json`, `_index.json`,
and Roo's fallback `claude_messages.json`. Common IDE/globalStorage paths are
probed only when those task files are present. Bob Shell `~/.bob` skill/state
paths are not imported by this adapter. Legacy installations that still keep
task data only inside VS Code state need an upstream/exported file-backed task
directory or an explicit `--path` once those files exist.

## Fidelity

An imported session may include messages, tool calls, command events, output
previews, file references, parent/child agent relationships, usage metadata, and
lifecycle events. Not every provider exposes every field.

Search output must identify the provider and cite the source path or cursor
when available so an agent can verify important details.
