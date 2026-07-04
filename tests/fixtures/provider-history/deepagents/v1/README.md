This fixture is a sanitized LangGraph checkpoint SQLite database generated with
`langgraph-checkpoint-sqlite` and `AsyncSqliteSaver`.

It mirrors the Deep Agents Code documented `~/.deepagents/.state/sessions.db`
table family (`checkpoints` plus `writes`) and contains one thread with user,
assistant, and tool messages in the root `writes.channel = 'messages'` stream.
No real Deep Agents user data or API credentials were used.
