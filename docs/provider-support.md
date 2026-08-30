# Provider Support

Supported is a functional product claim. For a provider's normal supported
version, the public CLI automatically locates an ordinary default native
history source, imports it through `ctx import --all`, and makes meaningful
user and assistant content available through search, show, and citations.
Repeating an unchanged import is a stable no-op without source failures or
duplicate records, and the route participates in the shared incremental,
read-only history architecture. Public deterministic tests exercise that
contract for every supported provider. Deeper route qualification is separate
from this functional support claim.

Support means backfilling history that the provider persists during ordinary
operation. Output from an installed runtime hook, opt-in exporter, or separately
configured sink does not qualify as provider history support.

The provider import policy in
[`provider-import-policy.md`](provider-import-policy.md) defines the native
storage families and the rules for real conversation text, tool output, raw
diffs, oversized rows, and fixtures.

Machine-readable provider metadata lives in
[`provider-support-matrix.json`](provider-support-matrix.json). Its
`tool_output` and `command_output` fidelity flags describe complete normalized
result bodies where the provider exposes them, subject only to the explicit
record-size and content policies. The public source formats below identify
discovery/import source shapes; stored event metadata may use the corresponding
per-file adapter format, such as
`codex_session_jsonl` for files discovered under `codex_session_jsonl_tree`.

Each row's `configured_root` object also records whether persistent named roots
are enabled and, when enabled, the required file or directory kind and the
provider's expansion strategy. `intentional_automatic_exact` means the provider
retains automatic
discovery plus one-shot exact import but does not accept `ctx sources add`.

Each provider row also has `lineage_support`. `session_relationship` is either
`exact_relationship` or `unknown`. `event_origin` distinguishes `exact_copy`,
`certified_prefix`, `explicit_no_copy`, and `unknown`. These values describe
what the shipped adapter admits into Core, not every feature the upstream tool
may expose. `unknown` does not mean unique, and similar transcript text never
upgrades it. A provider remains `unknown` until its adapter emits the typed
contract from stable structural data.

Custom History has a separate opt-in described in
[`custom-history-import-format.md`](custom-history-import-format.md). A durable
`provider_native_v1` source can state an exact relationship and exact copied
event selectors. Legacy files, unstable IDs, command-only plugins, and generic
ordered-prefix claims remain unknown.

General history support does not imply exact MCP activity attribution. That
event-local Core capability has its own provider + route + source format +
format version authority in
[`mcp-tool-call-attribution-capabilities.json`](mcp-tool-call-attribution-capabilities.json).
Capability revision 4 exact providers are Codex, Warp, and Copilot CLI. The
complete evidence matrix contains 48 base routes and 52 capability lanes:
three exact, 48 not-qualified, and one excluded. The Deep Agents hosted trace
is excluded from the local-only boundary, while its local SQLite history import
remains Supported but not qualified for exact attribution. See
[`mcp-tool-call-attribution.md`](mcp-tool-call-attribution.md) for absence,
privacy, migration, and retrieval semantics.

Current provider activity capture retains invocation/result channels at native
event granularity for the qualified tuples; it does not change general provider
support or the exact-attribution qualification counts.
See [`mcp-exchange-capture.md`](mcp-exchange-capture.md).

The public
support matrix is:

| Provider | Support | Source format |
| --- | --- | --- |
| Codex | Supported | `codex_session_jsonl_tree`, `codex_history_jsonl` |
| Grok Build | Supported | `grok_build_session_updates_jsonl_tree` |
| DeepSeek Harness | Supported | `deepseek_harness_session_jsonl_tree` |
| Pi | Supported | `pi_session_jsonl` |
| Claude Code | Supported | `claude_projects_jsonl_tree` |
| OpenCode | Supported | `opencode_sqlite` |
| Kilo Code | Supported | `kilo_sqlite` |
| MiMo Code | Supported | `mimocode_sqlite` |
| Kiro | Supported | `kiro_cli_sqlite` |
| Crush | Supported | `crush_sqlite` |
| Goose | Supported | `goose_sessions_sqlite` |
| Lingma | Supported | `lingma_sqlite` |
| Qoder | Supported | `qoder_transcript_jsonl_tree` |
| Warp | Supported | `warp_sqlite` |
| CodeBuddy | Supported | `codebuddy_history_json` |
| OpenClaw | Supported | `openclaw_agent_sqlite`, `openclaw_session_jsonl_tree` |
| Hermes Agent | Supported | `hermes_state_sqlite` |
| NanoClaw | Supported | `nanoclaw_project` |
| AstrBot | Supported | `astrbot_data_v4_sqlite` |
| Shelley | Supported | `shelley_sqlite` |
| Continue | Supported | `continue_cli_sessions_json` |
| OpenHands | Supported | `openhands_file_events`, `openhands_cli_file_events` |
| Antigravity | Supported | `antigravity_cli_transcript_jsonl_tree` |
| Gemini | Supported | `gemini_cli_chat_recording_jsonl` |
| Tabnine | Supported | `tabnine_cli_chat_recording_jsonl` |
| Cursor | Supported | `cursor_agent_transcript_jsonl_tree` |
| Zed | Supported | `zed_threads_sqlite` |
| GitHub Copilot | Supported | `copilot_cli_session_events_jsonl` |
| Factory AI Droid | Supported | `factory_ai_droid_sessions_jsonl` |
| Qwen Code | Supported | `qwen_code_chat_jsonl_tree` |
| Kimi Code | Supported | `kimi_code_cli_wire_jsonl_tree` |
| Auggie | Supported | `auggie_session_json` |
| Junie | Supported | `junie_session_events_jsonl_tree` |
| Firebender | Supported | `firebender_chat_history_sqlite` |
| XOPC | Supported | `xopc_sessions_sqlite` |
| ForgeCode | Supported | `forgecode_sqlite` |
| Deep Agents | Supported | `deepagents_sessions_sqlite` |
| Mistral Vibe | Supported | `mistral_vibe_session_jsonl_tree` |
| Mux | Supported | `mux_session_jsonl_tree` |
| Rovo Dev | Supported | `rovodev_session_json_tree` |
| Cline | Supported | `cline_sdk_session_store`, `cline_task_directory_json` |
| Roo Code | Supported | `roo_task_directory_json` |
| fx | Supported | `fx_sessions_tree` |

Codex session-tree discovery and exact `--path` import accept both ordinary
`.jsonl` rollouts and official standard-Zstandard `.jsonl.zst` rollouts. Both
representations derive source, session, and event identity from the embedded
Codex session UUID. If both representations of one native session coexist,
ctx selects raw JSONL first (then lexical path order) and publishes one logical
source; removing either representation does not change logical IDs.

Hermes Agent uses the native `hermes_state_sqlite` route. On Linux, a non-root
ctx process with the certified read-only live-WAL path makes new sessions and
appended records converge on native-watch and search refreshes. Where that fast
path is unavailable, incremental refresh defers without copying the provider
database. Structural edits, deletions, and deferred increments reconcile in
roughly 60–80 minutes with a healthy daemon, or on
`ctx import --provider hermes` or `ctx import --all`.

Factory AI Droid history is discovered automatically at `~/.factory/sessions`.
An exact path remains available, for example
`ctx import --provider factory-ai-droid --path /path/to/factory/sessions`.

Grok Build history is discovered at absolute `$GROK_HOME/sessions` when the
override is set, or `~/.grok/sessions` otherwise. Each admitted session
directory requires authoritative `updates.jsonl`; derived session sidecars
are not import authority. Exact `updates.jsonl` files can be selected with
`ctx import --provider grok-build --path /path/to/updates.jsonl`.

DeepSeek Harness is Supported for exact local format version 0 only. The
automatic winner is absolute `$DSH_HOME/sessions` for a nonempty
absolute override, otherwise `~/.dsh/sessions`; empty or whitespace-only
values are unset, while relative overrides require an explicit `--path`.
Default persistence uses nested `*/*/session.jsonl.zstd`, and configured raw
persistence uses nested `*/*/session.jsonl`. Exact leaves can be selected with
`ctx import --provider deepseek-harness --path /path/to/session.jsonl.zstd`.
Other format versions and layouts are unsupported, hosted/cloud history is out
of scope, and this import does not qualify exact MCP server/tool attribution.
Unknown required events and future format versions fail the source. Delegated
sessions remain independently searchable, but their immediate parent header
does not prove the transitive root identity required for typed lineage edges.

fx is Supported for legacy marker-less schema-v1/v2 `session.json` snapshots
accepted by current fx v0.0.6 and current schema-v3 transactional session
event logs. Discovery selects
`~/.fx/sessions`; named roots and exact imports accept a directory with the
same sessions-tree layout. For schema v3,
`authority.json` establishes event-log authority, `events.jsonl` is canonical
history, and a matching `commit.<generation>.json` watermark establishes the
committed boundary; `session.json` is only a projection. Pending commits and
uncommitted tails are excluded. Legacy snapshots are limited to 16 MiB. A
supported marker-less snapshot and the schema-v3 session created by its upstream
migration remain one logical fx session, so migration alone does not duplicate
history or rotate stable ctx identities. Marker-less schema-v3 snapshots,
future schemas, hosted history, and exact MCP server/tool attribution are not
supported. Current fidelity claims cover searchable user and assistant turn
content only; they do not claim normalized tool, command, file-touch, per-turn
model, or per-turn token fidelity.

`ctx sources --format json` reports each known provider source with `import_support`
and `importable` fields. A source is importable only when provider-specific
transcript files exist and match the documented format. NanoClaw participates in
native automatic import from an exact project CWD or official launchd/systemd
service registration; exact `--path` imports remain available for unregistered
project roots.

## Local Checks

Local checks exercise supported imports, provider filtering, citations, and
deterministic search without executing provider CLIs, reading real user history,
requiring API keys, or making network calls.
