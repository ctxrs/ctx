# Devin Provider History Fixture

Sanitized SQLite fixture for the Devin native history importer.

- Producer: Devin (Cognition), installed from the public `devin-cli`
  Homebrew cask. See Provenance below for the exact versions.
- Store: `~/.local/share/devin/cli/sessions.db`. Devin documents no environment
  variable for this location; `devin --help` exposes only `--config`. The
  fixture therefore proves the XDG-and-home default, not an override.
- Schema admission: structural, not version-pinned. The importer probes the
  tables, columns, and native unique keys it reads and refuses a source that
  loses any of them, while `max(refinery_schema_history.version)` feeds the
  capability digest so a migration rotates the published content digest. This
  fixture records version 17.

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
subagent_heads(session_id, agent_id, chain_node_id, updated_at)
                                             -- PK(session_id, agent_id)
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

## Sanitization and evidence boundary

The fixture preserves the native SQLite schema, forest shape, ordering,
metadata keys, and controlled oracle content while replacing prompts, local
inventories, paths, hostnames, and identifiers with fixture values. A final
deterministic publication pass also removed opaque reasoning signatures and
replaced host-private temporary-directory and summary-file names. Those
signatures are not imported by ctx. The fixture contains no credentials or
copied workspace code.

## Provenance

Two sessions were recorded by Devin CLI 3000.4.25 and one by 3000.6.14. Devin
CLI 3000.10.21 then migrated the database from schema version 16 to version 17,
which added `subagent_heads`. The public fixture's SHA-256 is
`a2f85bd32456d21d0fd7dc06619430cfb5f9bcda455972aed47e7eabb6092c69`.

The sessions are authentic Devin CLI output subsequently migrated to schema
version 17. The resulting `subagent_heads` table is empty because the recorded
sessions predate that migration. Focused tests add a temporary row to a private
copy of the fixture to exercise the current background-head contract without
misrepresenting synthetic data as a native capture. This fixture therefore
qualifies the historical native forest, compaction, foreground-subagent, and
tool-state shapes; current-writer background-subagent evidence remains a
separate qualification requirement.
