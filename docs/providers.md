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
- Dexto SQLite history from an explicit Dexto DB path;
- CodeBuddy JSON history under `~/.codebuddy`,
  `~/Library/Application Support/CodeBuddyExtension/Data`,
  `%LOCALAPPDATA%/CodeBuddyExtension`, or an explicit CodeBuddy history root;
- Aider Desk project task context files under `.aider-desk/tasks/<taskId>`,
  `AIDER_DESK_DIR/tasks/<taskId>`, or an explicit task, tasks, context file, or
  project root;
- OpenClaw session JSONL trees under `OPENCLAW_STATE_DIR`, `~/.openclaw`,
  legacy `~/.clawdbot`, or legacy `~/.moltbot`;
- Hermes Agent SQLite history under `HERMES_HOME/state.db` or
  `~/.hermes/state.db`;
- NanoClaw project history from a project root with `data/v2.db` and
  `data/v2-sessions` when imported explicitly;
- AstrBot local SQLite history from `ASTRBOT_ROOT/data/data_v4.db`,
  `~/.astrbot/data/data_v4.db`, or a project `data/data_v4.db` when imported
  explicitly;
- Shelley SQLite history from `SHELLEY_DB`, `~/.config/shelley/shelley.db`, or
  an explicit Shelley DB path;
- Continue CLI sessions from `CONTINUE_GLOBAL_DIR/sessions`,
  `~/.continue/sessions`, or an explicit Continue sessions path;
- OpenHands event JSON under `OH_PERSISTENCE_DIR`, legacy `FILE_STORE_PATH`,
  `~/.openhands`, or an explicit persistence root;
- Antigravity transcript JSONL mirrors under
  `~/.gemini/antigravity-cli/brain/*/.system_generated/logs/transcript_full.jsonl`
  or `transcript.jsonl`;
- Gemini CLI chat JSONL records under `~/.gemini/tmp/**/chats/**/*.jsonl`;
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
- ForgeCode conversation SQLite history from `FORGE_CONFIG/.forge.db`, legacy
  `~/forge/.forge.db`, `~/.forge/.forge.db`, or an explicit ForgeCode DB path;
- Mistral Vibe session directories under `VIBE_HOME/logs/session` or
  `~/.vibe/logs/session`, where each session has `meta.json` and
  `messages.jsonl`;
- Mux session transcripts under `MUX_ROOT/sessions` or `~/.mux/sessions`,
  where each workspace directory has `chat.jsonl` and optional `partial.json`
  plus archived subagent transcripts;
- Cline task JSON directories under `CLINE_DATA_DIR`, `CLINE_DIR/data`,
  `~/.cline/data`, or common VS Code globalStorage folders;
- Roo Code task JSON directories under `roo-cline.customStoragePath`, common
  VS Code globalStorage folders for `RooVeterinaryInc.roo-cline`, or an
  explicit path.

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
`kiro-cli`, `iflow-cli`, `forgecode`, `mistral-vibe`, `mux`, `zed`,
`codebuddy`, `aider-desk`, `cline`, and `roo`/`roo-code`.
Structured JSON and stable SQL views use provider IDs in ctx output; multiword IDs may be
snake_case, such as `copilot_cli`, `factory_ai_droid`, `qwen_code`,
`kimi_code_cli`, `autohand_code`, `kiro_cli`, `iflow_cli`, or
`mistral_vibe`; Aider Desk is reported as `aider_desk`, while compact native
IDs such as `kilo`, `openclaw`, `crush`, `goose`, `dexto`, `mux`, `zed`,
`codebuddy`, `forgecode`, `nanoclaw`, `astrbot`, `shelley`, `continue`, and
`openhands` stay compact. Roo Code is reported as `roo_code`.

`ctx sources --json` reports each known provider source with `import_support`
and `importable` fields. A native source is marked available/importable only
when provider-specific transcript files exist. Sources with `import_support:
"preview"` are explicit-import preview paths: use `ctx import --provider
nanoclaw` or `ctx import --provider astrbot` when discovery finds the desired
source, or add `--path` to target a specific source before searching it. They
are intentionally excluded from `ctx import --all` and pre-search refresh until
promoted. Sources with
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

## Cline And Roo Code Notes

Cline and Roo Code import support reads file-backed task directories, not VS
Code's private extension state databases. The importer looks for task folders
containing files such as `api_conversation_history.json`, `ui_messages.json`,
`task_metadata.json`, `history_item.json`, `_index.json`, and Roo's fallback
`claude_messages.json`. Common VS Code globalStorage paths are probed only when
those task files are present. Legacy installations that still keep task data
only inside VS Code state need an upstream/exported file-backed task directory
or an explicit `--path` once those files exist.

## Fidelity

An imported session may include messages, tool calls, command events, output
previews, file references, parent/child agent relationships, usage metadata, and
lifecycle events. Not every provider exposes every field.

Search output must identify the provider and cite the source path or cursor
when available so an agent can verify important details.
