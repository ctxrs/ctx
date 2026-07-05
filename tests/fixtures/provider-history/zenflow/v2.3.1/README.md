# Zenflow Desktop 2.3.1 SQLite fixture

`db.sqlite` was created from the local database schema emitted by the Zenflow
Desktop 2.3.1 Linux package, then populated with sanitized oracle rows. The
source package inspected for the schema was:

- `https://download.zencoder.ai/zenflowapp/latest/linux-x64/Zenflow.deb`
- SHA-256: `e623e073a212fccbfa295e2a7b7645a2c34525ab55f9cf247edce15babc731f2`

The fixture preserves the real table/column layout for `tasks`, `chats`,
`execution_processes`, `executor_sessions`, `assistant_sessions`,
`execution_process_logs`, and `execution_process_normalized_logs`. It contains
only synthetic prompts, log messages, IDs, paths, and model metadata.
