# Kode native JSONL fixture

This fixture is a sanitized Kode project-history tree for `@shareai-lab/kode@2.2.1` (`shareAI-lab/kode`).

Source anchors from the packed npm package:

- `dist/chunk-HGY32KZM.js` defines `${KODE_CONFIG_DIR:-~/.kode}/projects/<sanitized-cwd>/<sessionId>.jsonl`, where the project directory is `cwd.replace(/[^a-zA-Z0-9]/g, "-")`.
- The same file defines sidechain logs as `agent-<agentId>.jsonl` in the project directory and appends JSONL with `JSON.stringify(record) + "\n"`.
- Kode message records include `type`, `parentUuid`, `sessionId`, `cwd`, `uuid`, `timestamp`, `message`, optional `toolUseResult`, `isSidechain`, and `agentId`; source helpers also append `summary`, `custom-title`, `tag`, and `file-history-snapshot` rows.
- `dist/chunk-4OKQLS3L.js` defines the config fallback as `KODE_CONFIG_DIR ?? CLAUDE_CONFIG_DIR ?? ~/.kode`; the legacy global config file is commonly `~/.kode.json`.

The fixture uses `/workspace/kode` sanitized to `-workspace-kode`, with one primary session and one `agent-reviewer.jsonl` sidechain.
