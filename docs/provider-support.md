# Provider Support

Provider support is intentionally conservative. A provider is documented as
locally importable only when the public CLI can read existing local history for
that provider.

## Status Meanings

| Status | Meaning |
| --- | --- |
| `local_import` | The CLI can import an existing local history source for this provider. |
| `local_import_when_supported` | The CLI has an importer for a specific local format, but support depends on that file existing and matching the documented format. |
| `normalized_import_only` | Developer/test-only normalized provider JSONL exists, but this is not user-facing provider support. |
| `fixture_only` | The repository has sanitized fixture coverage, but the public CLI does not discover or import native local history for that provider. |
| `detected_unsupported` | The CLI can detect something about the provider but intentionally does not import it. |
| `blocked` | No shipped discovery or import path exists. |

## Current Matrix

Machine-readable provider metadata lives in
[provider-support-matrix.json](provider-support-matrix.json). The public truth
is:

| Provider | Status | Public import path | Public smoke |
| --- | --- | --- | --- |
| Codex | `local_import` | `~/.codex/sessions`, `~/.codex/history.jsonl`, or an explicit Codex path. | Static local-history fixture smoke. |
| Pi | `local_import_when_supported` | `~/.pi/sessions.jsonl` or an explicit Pi JSONL path. | Static local-history fixture smoke. |
| Claude | `local_import_when_supported` | `~/.claude/projects` or an explicit Claude projects JSONL tree. | Static local-history fixture smoke. |
| OpenCode | `local_import_when_supported` | `~/.local/share/opencode/opencode.db` or an explicit OpenCode SQLite DB. | Static local-history fixture smoke. |
| Antigravity | `local_import_when_supported` | `~/.gemini/antigravity-cli/brain` or an explicit Antigravity transcript JSONL tree. | Static local-history fixture smoke. |
| Gemini | `local_import_when_supported` | `~/.gemini` or an explicit Gemini CLI history tree. | Static local-history fixture smoke. |
| Cursor | `local_import_when_supported` | `~/.cursor/projects` agent transcript JSONL tree or an explicit Cursor agent transcript path. | Static local-history fixture smoke. |
| Copilot CLI | `local_import_when_supported` | `~/.copilot/session-state` or an explicit Copilot CLI session-state tree. | Static local-history fixture smoke. |
| Factory AI Droid | `local_import_when_supported` | `~/.factory/sessions` or an explicit Factory AI Droid sessions tree. | Static local-history fixture smoke. |

Fidelity fields in the machine-readable matrix describe the default public CLI
import behavior and normalized ctx storage fields. Codex command, patch, output,
and token details may be searchable or available in lower-level adapter modes,
but the public matrix does not currently claim normalized `tool_output`,
`command_output`, `files_touched`, or token-usage fields for default Codex
imports.

## Provider Smoke

Provider smoke coverage uses static local-history fixtures checked into the
repository. The public smoke target exercises supported imports, blocked
unsupported providers, provider filtering, citations, and deterministic search
without executing provider CLIs, reading real user history, requiring API keys,
or making network calls:

```bash
bazel test //:provider_fixture_e2e --config=ci
```

## Required Evidence For Promotion

Before a provider moves beyond `fixture_only`, `normalized_import_only`,
`detected_unsupported`, or `blocked` into native local-history support, the
change needs:

- a documented local source format;
- read-only source discovery or an explicit `--path` contract;
- malformed-input tests;
- idempotent re-import tests;
- source citation fields in search output;
- storage and redaction notes for provider-specific sensitive fields;
- docs updates in this file and `provider-support-matrix.json`.
