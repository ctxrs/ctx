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

`warp-mcp.sqlite` is a separately generated, fully sanitized fixture shaped from
Warp OSS commit `a93a68cff0d551fa7e4fb506852705c8a93f2c5b` and
`warp-proto-apis` commit `b0886a9523e2e05d102f61bd0a212dc15ade4835`.
It contains only oracle values and covers two UUID server identities exposing the
same tool, success/error/cancellation and text/nontext results, invalid server
IDs, required present args (including empty `Struct` messages), duplicate/orphan
linkage, call-ID reuse across tasks, and protobuf field-order/repetition/
unknown-field/oneof cases. A malformed-first/valid-second repeated embedded
message task verifies local rejection without suppressing neighboring valid
tasks.
