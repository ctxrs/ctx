# Provider Import Policy

This document is the contributor contract for native agent-history importers.
Provider adapters should make local history searchable and self-contained
without copying unused provider framing, binary payloads, or private blobs.

If an existing adapter conflicts with this policy, prefer fixing the adapter and
its fixtures over adding a new user-facing mode. The local CLI should have one
good default.

## Provider Location Policy

Discovery must be bounded, local, read-only, and offline. Reproduce the
provider's current precedence and select one authoritative root or profile,
not a union of override, default, old channel, migration, and fallback roots.
Within that selected root, import every bounded canonical history component
whose format is supported. A recognized unsupported component may coexist with
supported compatibility history and must not suppress it. Emit multiple roots
only for finite current stores that genuinely coexist. Within automatic
selection, an unreconstructible present replacement suppresses stale-default
discovery and requires exact `--path`.

One-shot flags, API paths, old working directories, copies, and host mounts are
explicit-path inputs. An explicit path bypasses discovery precedence only; it
does not bypass format admission, path safety, read bounds, identity, or
read-only database handling, and it is not remembered as a default.

Root support and format support are independent. Recognize genuinely
unsupported formats precisely and stop them before native dispatch, while
continuing to dispatch independent supported history selected by the same
bounded provider authority. Exact-path import is an escape hatch for moved,
copied, ambiguous, or unregistered locations; it is not a manual-only product
state for canonical history that ctx has already detected and can parse.

## Import Failure Policy

Every import keeps independently valid content. Human receipts present
record-local rejections in one stable `Skipped records` count; JSON keeps the
stable `rejected_records` fields and bounded diagnostic details. There is no
user-selectable failure mode. Parse, validation, and reference errors reject
the narrowest safe record plus records that depend on it. An
unreadable root, incompatible manifest/schema, or provider-database failure
fails that source while independent sources continue. Ctx-owned store, index,
worker, lock, and operational-I/O failures abort the run.

A source with accepted normalized units and deterministic record rejections
completes with `completed_with_rejections`. A structurally valid source that has
only session/source metadata or tool work also completes; it is not scanned a
second time merely to prove that it contains conversation text. A complete
deterministically rejected record advances the certified source frontier so it
cannot create a permanent hot retry. Incomplete records and retryable parser,
source, Store, or system failures do not advance it. Corrected complete records
remain eligible after source replacement is detected.

A fully published generation with deterministic record rejections and no source
failures is converged and searchable. Human import reports success and calls
those records skipped; status and doctor report the Core refresh as ready.
Source failures, retryable failures, an unpublished or retained generation, and
an unavailable verified search generation remain partial or unhealthy.

Content transactions use the shared 64-unit/8 MiB bounds with required WAL
checkpoints. Event-search merge suppression may span a whole source, including
manifested per-file imports, but its final compaction remains bounded.

## Default Import Shape

Native imports should store:

- real user, assistant, system, and developer conversation text;
- stable session, event, source, timestamp, model, cwd, and role
  metadata when available;
- file-touch metadata from structured provider fields or tool calls;
- complete structured tool inputs and results, including textual command
  output and patch/diff content within the Core admission bound;
- compact typed outcome/evidence such as status, exit code, exact commit IDs,
  and forge-review IDs in addition to the complete result content;
- stable ctx citations plus provider and session identity when available.

Native imports should not store or index by default:

- binary payloads, image payloads, screenshots, or provider-private blobs;
- usage/accounting records as message text;
- synthesized placeholder text such as `tool call: ...`, `message`, or
  `Event: ...` as a substitute for real provider-authored conversation text.

## Record-Local Content Gate

A supported native source is structurally imported in one pass. Real
conversation text is provider-authored text from an upstream message field with
a user, assistant, system, or developer role. It remains the highest-signal
search text, but its presence is not a source-level success gate.

The following do not become conversation text merely because their source is
otherwise valid, but their complete useful content may still be retained and
indexed as its actual Core event type:

- session headers, workspace rows, or metadata-only rows;
- tool calls, tool results, command output, raw diffs, patch bodies, usage
  records, permission changes, model changes, and lifecycle events;
- fallback text generated by the importer when the provider row has no
  human-readable message body.

Mixed rows are handled structurally. If one provider record contains an
assistant message plus a diff or tool payload, retain the complete accepted
fields without flattening them into a fake conversation message.

Metadata-only and tool-only sources may contribute sessions, explicit
relationships, file touches, complete command/tool content, and typed result
evidence. They must not synthesize placeholder message text to appear
searchable.

## Diffs And Tool Output

Diffs are edit artifacts, not conversation messages. Retain their complete
accepted text and changed-file metadata as structured tool content, and index
the meaningful text without inventing a message role.

Tool inputs and results are complete Core content. Parsers additionally retain
compact typed outcome/evidence, including exact linked commit or forge-review
identities when available. Query-time surfaces read committed Core records and
never reconstruct content from provider files.

## Native Retention Metadata

Provider capture reports the shared native content policy independently. A
selected record is complete; an explicit redaction or omission names its
versioned reason. Presentation limits are not persisted as retention policy.

```json
{
  "content": {
    "complete": true,
    "policy_status": "selected"
  }
}
```

- `complete: true` is valid only when every accepted field is present.
- `policy_status: selected` means no content policy redaction or omission was
  applied.
- Redacted or omitted records use `complete: false` and carry a stable,
  versioned policy reason.

There is no retained prefix mode. The 16-MiB Core record ceiling is an admission
bound: an indivisible over-bound record is rejected or handled by an explicitly
designed chunked representation, never silently truncated.

## Oversized Provider Rows

Provider-controlled text and blob fields must be bounded. One oversized value
should not prevent nearby valid messages from being indexed.

Adapters should:

- open provider databases and files read-only;
- enforce file, line, row, value, and decoded-payload limits appropriate to the
  storage family;
- skip or partially decode only the oversized field/row when possible;
- preserve safe source/session metadata and report an import failure diagnostic;
- avoid fabricating previews when the original value could not be read safely.

If a format stores several logical parts in one oversized opaque blob and the
adapter cannot safely separate the real message from the artifact, skip that
record and report the reason. Do not pretend the hidden data was indexed.

## Storage Families

Every supported native provider belongs to exactly one primary storage family.
Secondary traits are noted only to guide tests and hardening work.

| Provider | Source format(s) | Primary family | Notes |
| --- | --- | --- | --- |
| Codex | `codex_session_jsonl_tree`, `codex_history_jsonl` | JSONL transcript stream/tree | Session rollouts may be raw `.jsonl` or standard-Zstandard `.jsonl.zst`; exact compressed imports recover the embedded session UUID with a bounded catalog probe. Raw and compressed copies coalesce to one UUID-based source, preferring raw. Legacy prompt history remains raw JSONL. |
| Grok Build | `grok_build_session_updates_jsonl_tree` | JSONL transcript stream/tree | Session directories contain authoritative `updates.jsonl`; exact leaves use `grok_build_session_updates_jsonl`. Derived sidecars are excluded. |
| DeepSeek Harness | `deepseek_harness_session_jsonl_tree` | JSONL transcript stream/tree | Supported for local format version 0 only. Nested leaves use default `session.jsonl.zstd` or configured raw `session.jsonl`; exact leaves use `deepseek_harness_session_jsonl`. Hosted/cloud history is excluded. |
| Pi | `pi_session_jsonl` | JSONL transcript stream/tree | Single-provider JSONL sessions, including OMP-compatible paths. |
| Claude | `claude_projects_jsonl_tree` | JSONL transcript stream/tree | Project tree of JSONL transcripts. |
| OpenCode | `opencode_sqlite` | SQLite message store | Current schemas may split messages and parts. |
| Kilo Code | `kilo_sqlite` | SQLite message store | Current schemas may split messages and parts. |
| MiMo Code | `mimocode_sqlite` | SQLite message store | OpenCode-family sessions with messages and parts. |
| Kiro CLI | `kiro_cli_sqlite` | SQLite message store | SQLite conversation key/value rows containing message JSON. |
| Crush | `crush_sqlite` | SQLite message store | SQLite sessions with message parts and tool metadata. |
| Goose | `goose_sessions_sqlite` | SQLite message store | SQLite sessions/messages with structured content JSON. |
| Lingma | `lingma_sqlite` | SQLite message store | SQLite chat rows with prompt/assistant fields. |
| Qoder | `qoder_transcript_jsonl_tree` | JSONL transcript stream/tree | Transcript and direct project session JSONL leaves under the selected bounded project root. |
| Warp | `warp_sqlite` | SQLite encoded/blob store | SQLite rows include JSON plus decoded task protobuf blobs. |
| CodeBuddy | `codebuddy_history_json` | JSON session/task document | JSON history documents from editor state. |
| OpenClaw | `openclaw_session_jsonl_tree` | JSONL transcript stream/tree | Session tree with possible sidecar data. |
| OpenClaw | `openclaw_agent_sqlite` | SQLite transcript projection | Current per-agent database. Exact bounded v17 schema/owner admission is evaluated per normalized agent; admitted SQLite suppresses only that agent's legacy JSONL route, while corrupt/foreign databases fall back to JSONL and the two families are never admitted together. |
| Hermes Agent | `hermes_state_sqlite` | SQLite message store | SQLite sessions/messages with bounded exact reconciliation. |
| NanoClaw | `nanoclaw_project` | SQLite message store | Native project root containing central and per-session SQLite databases, discovered from exact CWD or official launchd/systemd service registration; exact `--path` remains available. |
| AstrBot | `astrbot_data_v4_sqlite` | SQLite message store | SQLite conversation/platform rows. |
| Shelley | `shelley_sqlite` | SQLite message store | SQLite conversations, messages, and tool rows. |
| Continue | `continue_cli_sessions_json` | JSON session/task document | JSON session files. |
| OpenHands | `openhands_file_events` | File event log | Legacy V1 and the selected current CLI route share the same certified conversation event-file storage family rather than a chat transcript table. |
| Antigravity | `antigravity_cli_transcript_jsonl_tree` | JSONL transcript stream/tree | Transcript tree. |
| Gemini | `gemini_cli_chat_recording_jsonl` | JSONL transcript stream/tree | Chat recording JSONL. |
| Tabnine | `tabnine_cli_chat_recording_jsonl` | JSONL transcript stream/tree | Chat recording JSONL. |
| Cursor | `cursor_agent_transcript_jsonl_tree` | JSONL transcript stream/tree | Agent transcript tree. |
| Zed | `zed_threads_sqlite` | SQLite encoded/blob store | SQLite thread rows with decoded JSON payloads. |
| Copilot CLI | `copilot_cli_session_events_jsonl` | JSONL transcript stream/tree | Session event JSONL. |
| Factory AI Droid | `factory_ai_droid_sessions_jsonl` | JSONL transcript stream/tree | Session JSONL. |
| Qwen Code | `qwen_code_chat_jsonl_tree` | JSONL transcript stream/tree | Chat JSONL tree. |
| Kimi Code CLI | `kimi_code_cli_wire_jsonl_tree` | JSONL transcript stream/tree | Wire-event JSONL tree. |
| Auggie | `auggie_session_json` | JSON session/task document | Single-session JSON. |
| Junie | `junie_session_events_jsonl_tree` | JSONL transcript stream/tree | Session event tree. |
| Firebender | `firebender_chat_history_sqlite` | SQLite message store | SQLite chat history. |
| ForgeCode | `forgecode_sqlite` | SQLite message store | SQLite sessions/messages. |
| Deep Agents | `deepagents_sessions_sqlite` | SQLite encoded/blob store | SQLite checkpoints/writes with decoded MessagePack values. |
| Mistral Vibe | `mistral_vibe_session_jsonl_tree` | JSONL transcript stream/tree | Session JSONL tree. |
| Mux | `mux_session_jsonl_tree` | JSONL transcript stream/tree | Session JSONL tree with active, archive, partial, and subagent history. |
| Rovo Dev | `rovodev_session_json_tree` | JSON session/task document | Session JSON tree. |
| Cline | `cline_sdk_session_store`, `cline_task_directory_json` | JSON session/task document | Current compound session catalog plus manifest/message artifacts; legacy task directory JSON remains separate. |
| Roo Code | `roo_task_directory_json` | JSON session/task document | Task directory JSON. |

Hermes `hermes_state_sqlite` is a supported SQLite message-store route with a
bounded consistency window. On Linux, a non-root ctx process with the certified
read-only live-WAL path makes new sessions and appended records converge on
native-watch and search refreshes. Where that fast path is unavailable,
incremental refresh defers without copying the provider database. Structural
edits, deletions, and deferred increments reconcile in roughly 60–80 minutes
with a healthy daemon, or on `ctx import --provider hermes` or
`ctx import --all`.

## Active-Writer Lifecycle Contract

Provider discovery and ingestion must remain read-only and must not lock,
truncate, rename, or copy an agent's live history merely to create a scan
boundary. Each scan records its read bound in ctx memory and applies the
contract for that source family:

- Append-log JSONL freezes each admitted file's EOF. On an ordinary exact-file
  watcher refresh, a provider that declares a live append-only contract may
  trust stable file identity plus growth and read only the new suffix. This
  makes continuous ingestion proportional to new bytes, but an arbitrary
  rewrite of old bytes is detected at an exhaustive reconciliation boundary,
  not necessarily by the next append event. Ambiguous watcher events, retries,
  truncation, path replacement, parser/checkpoint uncertainty, and explicit
  exhaustive refreshes disable that shortcut and authenticate the retained
  prefix. Daemon startup and watcher replacement are exhaustive safety
  boundaries as well. Only complete records ending at or before the frozen EOF
  are published; a partial final record is deferred until a later refresh
  completes it. Cold bootstrap and exhaustive refresh must not require the
  provider to become globally quiescent: once a leaf's opening observation is
  admitted, ordinary append-only growth of that retained object publishes the
  complete records inside the frozen prefix and defers the newer suffix. The
  next refresh imports that suffix exactly once, while truncation, path
  replacement, and mutation inside the authenticated prefix continue to fail
  closed.
- Standard-Zstandard Codex rollouts snapshot exactly the frozen compressed
  prefix from the retained source handle, then decode and hash that same private
  snapshot. The compressed snapshot plus decoded spool is bounded to 256 MiB
  per leaf; a 256x plus 16 MiB expansion bound and 128 MiB decoder-window bound
  apply inside that ceiling. At most four maximum-size compressed leaves run in
  parallel against the shared 1 GiB route scratch budget. Corrupt, truncated,
  over-bound, or budget-unavailable streams fail closed.
- SQLite is observed as one short read-only logical snapshot. WAL size,
  checkpointing, and physical database-file size are not ingestion states.
  Concurrent committed rows belong to a subsequent certified snapshot unless
  they are visible inside the admitted transaction. On platforms and routes
  that cannot retain a no-write snapshot directly, ctx first preallocates and
  streams one private DB/WAL copy below its data root. Admission depends on
  available temporary disk capacity plus safety headroom, not a fixed source
  size ceiling; copy memory remains bounded and the private family is removed
  when the route finishes.
- JSON documents and document trees use unchanged-or-replace semantics. A
  mutation during a scan invalidates that candidate; the retry reads one
  complete replacement.
- Event-file trees treat admitted event files as immutable leaves. New leaves
  are discovered by the next inventory, while a changed or replaced admitted
  leaf invalidates its old evidence.

Tests are layered by responsibility: each provider owns parsing, identity, and
normalized-content fixtures; the shared source-family suite owns active append,
partial-tail, rewrite, truncation, replacement, logical-snapshot, and
inventory-race behavior. Platform suites separately prove retained-handle,
symlink/reparse, read-only SQLite, watcher, and daemon behavior. This avoids a
provider-by-behavior-by-platform Cartesian test matrix while ensuring an active
agent is covered regardless of which supported provider it uses.

## Family-Specific Fixture Expectations

JSONL transcript stream/tree adapters should test malformed lines, oversized
lines, missing session headers when the format permits them, metadata-only
streams, tool-only streams, duplicate cursors, and nested/tree discovery.

JSON session/task document adapters should test empty task metadata, missing
message arrays, mixed message plus artifact payloads, oversized files or nested
fields, and idempotent re-import.

SQLite message-store adapters should test missing tables/columns, schema
fingerprints, metadata-only sources, tool-only sources, mixed real messages plus
tool rows, oversized text cells, and read-only opening behavior.

SQLite encoded/blob-store adapters should test corrupt encoded payloads,
oversized blobs, decoded-message extraction, metadata-only rows, tool-only
payloads, and sensitive-token non-copying.

File event-log adapters should test event ordering, truncated/corrupt files,
metadata-only runs, complete structured diff/output retention, and stable ctx
citations with provider/session identity.

Custom history importers and history-source plugins use the public
`ctx-history-jsonl-v2` contract instead of a native provider storage family, but
they should follow the same content policy: complete accepted conversation and
structured tool content is stored, meaningful text is indexed, and unsupported
binary/provider-private material is explicitly omitted.
