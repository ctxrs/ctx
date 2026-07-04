# Terramind agents.db fixture

This fixture is a sanitized SQLite database for `terramind@0.2.91`.

Primary source proof:

- `npm view terramind@0.2.91` reports repository
  `https://github.com/terramind-io/ide`, homepage `https://terramind.com`,
  and CLI bin `terramind.cjs`.
- The published `terramind-0.2.91.tgz` contains `migrations/0000_mixed_blur.sql`,
  which creates `projects`, `chats`, and `sub_chats`; it also contains
  `migrations/meta/0035_snapshot.json`, whose final schema includes
  `tool_outputs`.
- The bundled `package/terramind.cjs` source map comments identify
  `src/main/lib/db/index.ts`; inspected bundle lines near `6530` resolve the DB
  as `<getUserDataPath()>/data/agents.db`. In the CLI context, inspected lines
  near `116273` implement `getCliUserDataPath()` using `$XDG_CONFIG_HOME/Nucleus`
  or `~/.config/Nucleus` on Linux, `~/Library/Application Support/Nucleus` on
  macOS, and `%APPDATA%/Nucleus` on Windows.
- The bundled schema code near `package/terramind.cjs:5932-6109` defines
  `projects`, `chats`, `sub_chats`, and `tool_outputs`, with
  `sub_chats.messages` as text defaulting to JSON `[]`.
- Runner code near `package/terramind.cjs:135909` and
  `package/terramind.cjs:136947` parses/stringifies `sub_chats.messages` JSON
  arrays and stores large stripped outputs in `tool_outputs(full_output)`.

No-auth generation gap:

- A temporary-home `npx terramind@0.2.91 list --chats` probe did not complete
  within the local check window and was stopped, so this fixture is created
  from the package's published migration/schema evidence rather than from a
  live Terramind run.
