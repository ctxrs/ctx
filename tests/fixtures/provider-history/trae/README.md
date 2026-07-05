# Trae fixture provenance

This fixture is a source-backed synthetic `state.vscdb`, not a real local Trae
run export. No Trae binary or local Trae data root was available on the fixture
authoring machine.

The SQLite shape follows public proof from `yuanjing001/trae-chats-exporter` at
commit `85e2d111a5a0e35f0957502097d3a8b18095ef72`, which reads
`User/workspaceStorage/<workspace>/state.vscdb` `ItemTable` values for keys such
as `memento/icube-ai-agent-storage`, `chat.ChatSessionStore.index`, and
`ChatStore`.

The chat content and workspace path are synthetic oracle strings for parser and
CLI tests.
