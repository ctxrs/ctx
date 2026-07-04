# Lingma Provider History Fixture

This directory contains sanitized SQLite fixtures for the Lingma native history
importer. They model only the source-backed contract proven by WayLog:

- Upstream proof: `shayne-snap/WayLog@6939033b7a39326fbdc249e28e6aa12461db1f09`
  `src/services/readers/lingma-reader.ts`
  (`https://raw.githubusercontent.com/shayne-snap/WayLog/6939033b7a39326fbdc249e28e6aa12461db1f09/src/services/readers/lingma-reader.ts`).
- Default paths proven by that reader:
  `~/.lingma/vscode/sharedClientCache/cache/db/local.db` and
  `~/.lingma/vscode-insiders/sharedClientCache/cache/db/local.db`.
- SQLite table proven by that reader:
  `chat_record(session_id, request_id, chat_prompt, summary, error_result,
  gmt_create, extra)`.

WayLog describes the Lingma reader as summaries-only, and comments that original
answer text may be encrypted. The ctx fixture therefore imports `chat_prompt`
as user text and `summary`/meaningful `error_result` as partial assistant
content. Tests should not treat Lingma assistant rows as full-fidelity answer
transcripts unless a stronger source proves a richer local contract.

Alibaba public product docs say Qoder CN is the renamed Lingma product line, but
this fixture does not prove a Qoder CN DB path or identical local schema. ctx
therefore documents the overlap without adding a `qoder-cn` provider alias.
Relevant docs anchors:
`https://www.alibabacloud.com/help/zh/lingma/product-overview/introduction-of-lingma`
and
`https://www.alibabacloud.com/help/en/lingma/product-overview/qoder-cn-update-log`.
