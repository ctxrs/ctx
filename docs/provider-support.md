# Provider Support Matrix

This branch proves provider integration through explicit, local import commands.
It does not install passive Codex, Claude, or Pi hooks, and it does not scan
provider directories automatically.

| Provider | Implemented path | Source format | Fidelity | Captured | Not captured | Proof |
| --- | --- | --- | --- | --- | --- | --- |
| Codex | `ctx capture import-provider --provider codex --input <fixture.jsonl>` | normalized provider fixture JSONL | imported | sessions, events, exposed parent-child session edges, cursors, source metadata | native transcript discovery, assistant/tool fidelity beyond fixture content | `tests/fixtures/provider/codex.jsonl`; capture and CLI tests |
| Codex | `ctx capture import-codex-history --input ~/.codex/history.jsonl` | Codex prompt history JSONL with `session_id`, `ts`, `text` | summary_only | user prompt events grouped by Codex session id | assistant replies, tool calls, command output, artifacts, child session relationships | `tests/fixtures/provider-history/codex-history.jsonl`; capture and CLI tests; local blocker notes below |
| Claude | `ctx capture import-provider --provider claude --input <fixture.jsonl>` | normalized provider fixture JSONL | imported | sessions, events, cursors, source metadata present in fixture | native Claude Code history discovery, hooks, live transcript capture | `tests/fixtures/provider/claude.jsonl`; capture and CLI tests |
| Pi | `ctx capture import-provider --provider pi --input <fixture.jsonl>` | normalized provider fixture JSONL | imported | sessions, events, source metadata present in fixture, secret-key redaction in metadata | native Pi history discovery, hooks, live transcript capture | `tests/fixtures/provider/pi.jsonl`; capture and CLI tests |

## Source and Fidelity Metadata

Imported provider rows record `source_format` in sync metadata. Normalized
fixture imports use `normalized_provider_fixture_jsonl` and `fidelity=imported`.
Codex prompt-history imports use `codex_history_jsonl` and
`fidelity=summary_only`.

The Codex history path intentionally does not create parent/child edges. The
local history format observed for this branch exposes prompt log rows only, not
subagent relationships.

## Local E2E Evidence and Blockers

Local provider inventory on 2026-06-23:

- Codex: `/home/daddy/.codex/history.jsonl` exists and contains prompt-history
  JSONL rows with `session_id`, `ts`, and `text`. This validates the gated
  parser shape, but the real file is private local data and was not committed.
- Claude: `/home/daddy/.claude` was not present, so native Claude history E2E
  could not run.
- Pi: `/home/daddy/.pi/agent/auth.json` was present, but no local Pi transcript
  or history file was found under `/home/daddy/.pi` within four directory
  levels, so native Pi history E2E could not run.

Gated real-data check for Codex:

```bash
CTX_DATA_ROOT="$(mktemp -d)" \
  ctx capture import-codex-history --input "$HOME/.codex/history.jsonl" --json
```

Run this only on a machine where importing local prompt history into a temporary
ctx data root is acceptable. The output should report `source_format:
codex_history_jsonl`, `fidelity: summary_only`, and limitations explaining that
assistant replies, tools, command output, and child sessions are not present.

Gated real-data checks for Claude and Pi remain blocked until their native
history locations and schemas are available. Until then, only the normalized
fixture adapters are proven.
