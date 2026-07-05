# Warp SQLite fixture

This sanitized fixture was generated locally for ctx tests; it is not copied from
a user Warp profile. It contains the public Warp SQLite tables
`agent_conversations`, `agent_tasks`, and `ai_queries`, with one
`agent_tasks.task` protobuf blob shaped from the public
`warpdotdev/warp-proto-apis` `apis/multi_agent/v1/task.proto` schema.

The fixture text uses oracle strings only:

- `warp sqlite oracle prompt`
- `Warp sqlite oracle answer`

The `conversation_data` row includes a dummy server conversation token to assert
that ctx records only boolean token presence metadata and does not copy cloud
sync tokens into normalized history.
