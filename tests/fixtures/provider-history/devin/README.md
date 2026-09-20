# Devin Provider History Fixture

Sanitized SQLite fixture for the Devin native history importer.

- Producer: Devin (Cognition), installed from the public `devin-cli`
  Homebrew cask. See Provenance below for the exact versions recorded.
- Store: `~/.local/share/devin/cli/sessions.db`. Devin documents no environment
  variable for this location; `devin --help` exposes only `--config`. The
  fixture therefore proves the XDG-and-home default, not an override.
- Schema admission: structural, not version-pinned. The importer probes the
  tables, columns, and indexes it reads and refuses a source that loses any of
  them, while `max(refinery_schema_history.version)` feeds the capability
  digest so a migration rotates the published content digest. This fixture
  records version 17.

## Ground truth

Tables observed in the live store, all reproduced here verbatim in shape:

```text
refinery_schema_history(version, name, applied_on, checksum)
sessions(id, working_directory, backend_type, model, agent_mode, created_at,
         last_activity_at, title, main_chain_id, shell_last_seen_index,
         cogs_json, workspace_dirs, hidden, metadata)
message_nodes(row_id, session_id, node_id, parent_node_id, chat_message,
              created_at, metadata)          -- UNIQUE(session_id, node_id)
tool_call_state(session_id, tool_call_id, tool_call_json,
                tool_call_update_json)       -- PK(session_id, tool_call_id)
prompt_history, rendered_commits, app_state  -- present, left empty here
```

Facts the importer depends on, each verified against the live store:

- `message_nodes` per session is a **forest**, not a chain. The primary
  transcript is the walk from `sessions.main_chain_id` up `parent_node_id`,
  reversed. Most nodes are off that chain.
- Timestamps are unix **seconds** (`created_at`), with an RFC3339
  `chat_message.metadata.created_at` alongside them.
- `chat_message` is a JSON **object** (not an array), with keys `role`,
  `content`, `message_id`, `metadata`, and optionally `tool_calls`,
  `tool_call_id`, and `thinking`. Roles observed: `user`, `assistant`, `tool`,
  `system`.
- Vendor extension blobs hang off `chat_message.metadata.extensions` under
  slash-namespaced keys. The sixteen present in this fixture are
  `chisel/tool_result_meta`, `chisel/tool_call_timing`,
  `chisel/tool_call_content`, `chisel/undo`, `chisel/terminal_output`,
  `chisel/acp-content-blocks`, `chisel/client-message-id`,
  `agent-ext/title-source-text`, `agent-ext/rules-loaded`,
  `agent-ext/skills-loaded`, `affogato/cog-context`, `devin-rs/summary`,
  and the four `subagent/*` keys below.
- A **linked subagent thread** is recorded only on the tool node that reports a
  foreground `run_subagent` result, which carries
  `chat_message.metadata.extensions` keys `subagent/chain_node_id` (the tip of
  the subagent's own chain), `subagent/agent_id`, `subagent/profile_name`, and
  `subagent/model`. Note the location: these are in the *chat message's*
  extensions, not in `message_nodes.metadata.extensions`.
- **Compaction writes a pair of nodes**, both carrying the same
  `message_nodes.metadata.summarized_from`. One is an `assistant` node holding
  the generated `<summary>`, produced by an off-chain summarizer thread. The
  other is a `system` node beginning "You are continuing work from a previous
  conversation thread" and is the one that lands on the new chain. Splicing is
  transitive: a spliced segment can itself contain a node with
  `summarized_from`.
- `message_nodes.metadata` carries only `summarized_from`,
  `num_tokens_preceding`, `is_system_prefix`, and an `extensions` object whose
  sole observed key is `compact/prior_node_ids`.
- `tool_call_state.tool_call_json` is documented nullable, and many `tool`
  nodes have no `tool_call_state` row at all, so enrichment must be optional.
- `tool_call_state` blobs are bare ACP `ToolCall` / `ToolCallUpdate` values.
  Vendor fields appear under the ACP `_meta` namespace, e.g.
  `cognition.ai/cwd` and `cognition.ai/preview_is_shell_command`.

## Contents

`v17/sessions.db` holds three real sessions, all recorded in
`/tmp/devin-fixture` specifically to serve as importer oracles:

| Session | Nodes | Roots | Chain | `tool_call_state` |
| --- | --- | --- | --- | --- |
| `abounding-crest` | 31 | 4 | `main_chain_id = 30`, 11 nodes | 1 |
| `exclusive-bamboo` | 34 | 4 | `main_chain_id = 33`, 13 nodes | 2 |
| `discovered-sandal` | 49 | 9 | `main_chain_id = 48`, 5 nodes, 14 after the splice | 2 |

76 of the 114 nodes are off-chain, which is the point of the fixture. It covers:

- A multi-root forest: four to nine system-prefix trees per session.
- Duplicated system-prefix subtrees carrying `compact/prior_node_ids`
  back-references to the nodes they copy.
- Abandoned regenerated turns (`abounding-crest` nodes 26 and 29 sit beside the
  retained 27 and 30 under the same parent; `discovered-sandal` has four such
  pairs), which the walk must exclude without disturbing the chain.
- A main chain that runs *through* a system-prefix tree, so system prompts are
  on-chain rather than skippable.
- `tool` nodes both with and without a matching `tool_call_state` row.
- **Compaction.** `discovered-sandal` node 48 is the on-chain `system` node
  with `summarized_from = 42`, and node 46 is the off-chain `assistant`
  `<summary>` sharing that value. Its base chain is only 5 nodes and becomes 14
  once the splice is followed, so an importer that ignored `summarized_from`
  would silently drop almost the whole session.
- **A summarizer thread.** `discovered-sandal` nodes 43 to 46 form a separate
  tree that produced that summary. It is counted, not imported.
- **A linked subagent thread.** `discovered-sandal` node 37 is an on-chain
  `tool` node carrying `subagent/chain_node_id = 35`,
  `subagent/agent_id = d8a8ea4c`, and `subagent/profile_name = Explore`. The
  subagent's own chain is 4 nodes rooted at system node 31 and shares no node
  with the primary chain.

Oracle strings, used by the CLI conformance tests:

- `abounding-crest`: user asks for `echo devinclitooloracle`; the tool node
  carries `devinclitooloracle`; the assistant replies
  `devincliassistantoracle`.
- `exclusive-bamboo`: user asks for a file edit; the tool node reports writing
  `devinclifileoracle` to `note.txt`; the assistant replies `devincliedited`.

## Sanitization

Following the convention set by the `warp`, `cursor`, and `antigravity`
fixtures, this fixture preserves native field names, keys, and graph shape while
carrying no vendor system-prompt prose, no operator-private prompt text, and no
copied workspace code. Every node, `node_id`, `parent_node_id`, `message_id`,
role, metadata key, and extension key is reproduced as observed. The following
text bodies are replaced with synthetic placeholders:

- The six Devin system-prefix segments (`agent-instructions`,
  `subagent-profiles`, `model-identity`, `parallel-tool-calls`,
  `subagent-instructions`, `summarizer-instructions`), wherever they appear —
  in `message_nodes.chat_message.content`, in
  `sessions.cogs_json[].append_system_messages[].content`, and in
  `sessions.cogs_json[].set_system_prefix[].content`.
- The summarizer thread's two prompt nodes: the request itself, and the input
  node, which is a verbatim re-dump of the whole conversation and therefore
  embeds the system prompts a second time.
- The operator's always-on rule text and its path, replaced by a synthetic
  `devinclirule` rule at `/home/dev/.devin/rules/devinclirule.md`.
- The `<available_skills>` listing, replaced by a synthetic
  `devinclioracleskill` entry.
- The `agent-ext/rules-loaded` and `agent-ext/skills-loaded` inventories, whose
  entries enumerate whatever the operator happens to have installed, including
  local paths and third-party skill descriptions. Both are replaced wholesale
  with one synthetic entry so the extension shape survives and the inventory
  does not.
- `/home/ubuntu` throughout, rewritten to `/home/dev`, and one employer-internal
  hostname, rewritten to `example.invalid`.

The generated compaction summary and the tool output are kept as recorded: they
describe the fixture's own `/tmp` disk-usage task and contain nothing private,
and the on-chain summary node's text is what the importer projects as a summary
event.

Replacements are applied to parsed JSON values and re-serialized, so they cannot
corrupt structure. The result was re-scanned to confirm none of the original
strings survive.

## Provenance

`abounding-crest` and `exclusive-bamboo` were recorded against Devin
`3000.4.25`; `discovered-sandal` was recorded against `3000.6.14` with a prompt
chosen to force a foreground `run_subagent` followed by `/compact`, so that the
subagent-linkage and compaction shapes come from a real run rather than from
hand-written SQL. Both producer versions write schema version 16.

Devin `3000.10.21` then migrated the store to version 17, adding one table:

```sql
CREATE TABLE subagent_heads (
    session_id    TEXT    NOT NULL,
    agent_id      TEXT    NOT NULL,
    chain_node_id INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    PRIMARY KEY (session_id, agent_id),
    FOREIGN KEY (session_id) REFERENCES sessions(id)
);
```

Nothing else changed: the four tables the importer reads are byte-identical
across 16 and 17. This fixture was migrated in place with that exact DDL and
the real `refinery_schema_history` row, rather than re-recorded, so the
sanitized session content above is unchanged and still traceable to the runs
that produced it.

`subagent_heads` is empty here, which is what an in-place migration produces:
Devin writes a row when a session spawns a subagent after the migration, and
all three sessions predate it. The importer does not read the table. It is an
authoritative `(session_id, agent_id)` to `chain_node_id` map, so it is the
missing evidence for the background subagent threads this importer currently
declines to claim lineage for.
