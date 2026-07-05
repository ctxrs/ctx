Firebender fixture derived from the public JetBrains Marketplace plugin
artifact `Firebender 1.0.10` (`updateId=1045537`).

The obfuscated `ChatHistoryDatabaseService` string decoder resolves the
project-local path `.idea/firebender/chat_history.db` and creates a
`chat_sessions` table with `id`, `name`, `created_at`, `updated_at`,
`deleted_at`, `messages_json`, and `metadata_json` columns. The plugin also
creates `schema_info` and `subagent_conversations`.

Message model bytecode in the same artifact exposes serialized fields such as
`role`, `content`, `tool_calls`, `tool_call_id`, `type`, and `text`.
