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

| Provider | Status | Public import path | Live E2E lane |
| --- | --- | --- | --- |
| Codex | `local_import` | `~/.codex/sessions`, `~/.codex/history.jsonl`, or an explicit Codex path. | Manual opt-in local-history smoke. |
| Pi | `local_import_when_supported` | `~/.pi/sessions.jsonl` or an explicit Pi JSONL path. | Manual opt-in local-history smoke. |
| Claude | `detected_unsupported` | Native `.claude/projects` import is blocked until a read-only parser and native fixtures ship. | No live lane; native importer required first. |
| OpenCode | `detected_unsupported` | Native `opencode.db` or export import is blocked until a read-only parser and native fixtures ship. | No live lane; native importer required first. |
| Antigravity | `detected_unsupported` | Native import is blocked until a stable local transcript path/schema is proven. | No live lane; native importer required first. |
| Gemini | `detected_unsupported` | Native session/checkpoint import is blocked until a parser and native fixtures ship. | No live lane; native importer required first. |
| Cursor | `detected_unsupported` | Native import is blocked until persisted local DB/files and a read-only parser are proven. | No live lane; native importer required first. |
| Copilot CLI | `detected_unsupported` | Native session-state/session-store import is blocked until schemas, redaction, and read-only fixtures ship. | No live lane; native importer required first. |
| Factory AI Droid | `detected_unsupported` | Native import is blocked because no stable durable local transcript path/schema is proven. | No live lane; native importer required first. |
| Amp | `detected_unsupported` | Native local thread import is blocked because no stable local thread file path/schema is proven. | No live lane; native importer required first. |

Fidelity fields in the machine-readable matrix describe the default public CLI
import behavior and normalized ctx storage fields. Codex command, patch, output,
and token details may be searchable or available in lower-level adapter modes,
but the public matrix does not currently claim normalized `tool_output`,
`command_output`, `files_touched`, or token-usage fields for default Codex
imports.

## Manual Live E2E

Live provider E2E is not part of default CI. It is a manual, non-publishing,
opt-in proof surface.

Codex and Pi can run local-history import proof because they have documented
native local source formats. Required guardrails:

- set `CTX_LIVE_PROVIDER_E2E=1`;
- set `CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY=1`;
- select `CTX_LIVE_PROVIDER_CODEX=1` or `CTX_LIVE_PROVIDER_PI=1`;
- provide `CTX_LIVE_PROVIDER_CODEX_SESSIONS_PATH` or
  `CTX_LIVE_PROVIDER_PI_SESSIONS_PATH`;
- provide a provider-specific query variable or `CTX_LIVE_PROVIDER_QUERY` for
  deterministic retrieval-oracle hits;
- use a temporary `CTX_DATA_ROOT`;
- do not execute provider CLIs;
- do not pass API-key environment variables to `ctx`;
- write only redacted aggregate/oracle-count `live-e2e.json` and `live-e2e.md`
  artifacts.

Codex may also set `CTX_LIVE_PROVIDER_CODEX_HISTORY_PATH`. Raw configured
queries must not be written to artifacts.

The artifacts intentionally omit raw transcripts, snippets, queries, and source
paths. Passing Codex and Pi artifacts include only aggregate import, retrieval,
provider-filter, citation, `source_exists`, and health oracle counts.
Fixture-only providers write blocked artifacts until a native read-only local
importer ships.

OpenRouter-generated histories may be used only as a developer drafting aid for
static fixture work. They are not a default Bazel target, CI target, Buildkite
step, provider-live lane, release-contract requirement, live test gate, or
native local-history proof.

The Bazel provider-live wrapper does not build `ctx` for skipped or fixture-only
blocker lanes. A true Codex or Pi local-history live run may build or use the
selected `ctx` binary, but the runtime flow invokes only `ctx setup`, `ctx
import`, `ctx search`, `ctx status`, `ctx doctor`, and `ctx validate` with a
scrubbed environment. Provider CLIs, provider API keys, and
provider network credentials are not used by those lane commands.

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
- a redacted live E2E artifact when claiming live local-history support;
- docs updates in this file and `provider-support-matrix.json`.
