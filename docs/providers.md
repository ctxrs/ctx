# Providers

ctx imports existing agent history through provider adapters. Each adapter must
make a narrow, testable claim about the source format it reads and the event
fields it indexes.

## Supported Local Imports

The current CLI imports local history for:

- Codex session JSONL trees under `~/.codex/sessions`;
- Codex `~/.codex/history.jsonl`;
- Pi `~/.pi/sessions.jsonl` when that local file exists and matches the
  supported JSONL format;
- Claude Code project JSONL transcripts under `~/.claude/projects`;
- OpenCode SQLite history under `~/.local/share/opencode/opencode.db`;
- Antigravity transcript JSONL mirrors under
  `~/.gemini/antigravity-cli/brain/*/.system_generated/logs/`;
- Gemini CLI chat JSONL records under `~/.gemini`;
- Cursor CLI agent transcript JSONL files under `~/.cursor/projects`;
- Copilot CLI session event logs under `~/.copilot/session-state`;
- Factory AI Droid session JSONL files under `~/.factory/sessions`.

Use `ctx sources` for the truth on the current machine:

```bash
ctx sources
ctx sources --json
```

`ctx sources --json` reports each known provider source with `import_support`
and `native_import` fields. Sources with `import_support: "unsupported"` are
detections or blockers, not importable native history.

## Developer Normalized Inputs

The CLI has a developer/test-only normalized provider JSONL input. Set
`CTX_PROVIDER_NORMALIZED_IMPORT_DEV=1` when using that input. It is for adapter
harnesses, generated static fixture drafting, and future native importer
development. It is not native provider history discovery or user-facing
provider support.

If a provider is selected without a proven native importer, `ctx import`
returns a provider-specific native-history blocker. Do not document a provider
as natively locally importable until the CLI can discover or parse that
provider's real local history and the provider support matrix marks the shipped
path accordingly.

## Provider Smoke

Public provider smoke coverage uses static local-history fixtures. It verifies
supported imports, unsupported-provider blockers, provider filtering, citations,
and deterministic search without executing provider CLIs, reading real user
history, requiring API keys, or making network calls:

```bash
bazel test //:provider_fixture_e2e --config=ci
```

## Import Rules

Provider imports should be:

- read-only with respect to provider-owned files;
- explicit through `ctx import`;
- safe to interrupt and re-run, using idempotent rescans or provider cursors
  when available;
- idempotent for unchanged source files;
- clear about which fields were indexed and which were left raw-only;
- conservative when a transcript schema is unknown or malformed.

## Fidelity

An imported session may include messages, tool calls, command events, output
previews, file references, parent/child agent relationships, usage metadata, and
lifecycle events. Not every provider exposes every field.

Search output must identify the provider and cite the source path or cursor
when available so an agent can verify important details.
