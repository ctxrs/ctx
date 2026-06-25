# Provider Support

Provider support is intentionally conservative. A provider is documented as
locally importable only when the public CLI can read existing local history for
that provider.

## Status Meanings

| Status | Meaning |
| --- | --- |
| `local_import` | The CLI can import an existing local history source for this provider. |
| `local_import_when_supported` | The CLI has an importer for a specific local format, but support depends on that file existing and matching the documented format. |
| `normalized_import_only` | The CLI can import explicit normalized provider JSONL for harnesses and adapters, but does not discover or parse the provider's native local history. |
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
| Claude | `normalized_import_only` | Explicit normalized provider JSONL path only; no native local-history discovery. | Manual opt-in generated-history smoke. |
| OpenCode | `normalized_import_only` | Explicit normalized provider JSONL path only; no native local-history discovery. | Manual opt-in generated-history smoke. |
| Antigravity | `normalized_import_only` | Explicit normalized provider JSONL path only; no native local-history discovery. | Manual opt-in generated-history smoke. |
| Gemini | `normalized_import_only` | Explicit normalized provider JSONL path only; no native local-history discovery. | Manual opt-in generated-history smoke. |
| Cursor | `normalized_import_only` | Explicit normalized provider JSONL path only; no native local-history discovery. | Manual opt-in generated-history smoke. |

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

The generated OpenRouter lane is separate from native local-history proof. It
uses `scripts/run-openrouter-provider-e2e-infisical.sh` to hydrate OpenRouter
credential and endpoint configuration from Infisical before the Bazel target
generates temporary synthetic histories for Codex, Pi, Claude, OpenCode,
Antigravity, Gemini, and Cursor. On Buildkite runners where the agent hook has
already hydrated OpenRouter env from Infisical, the same wrapper uses that
pre-hydrated environment instead of requiring an `infisical` binary on `PATH`.
Buildkite invokes the Bazel target through `scripts/check.sh -- test`, so the
same Bazel/Bazelisk bootstrap path is used as the main CI gate. The lane passes
a deterministic non-secret OpenRouter model override to the Bazel test
environment because Buildkite runner hooks pre-hydrate credentials but not model
names; the generator has an optional free-model guard for projects whose
OpenRouter provider policy permits free aliases.
Then it runs only `ctx setup`, `ctx import`, `ctx search`, `ctx status`, `ctx
doctor`, and `ctx validate` with a scrubbed environment. The
credential is not passed to `ctx`, generated raw histories are not persisted as
artifacts, and the lane writes redacted per-provider evidence under
`generated-providers/<provider>/` plus an aggregate summary. `source_exists`
counts are not required for those temporary histories, and the lane stays out
of default `production` and `release_contract` gates.

The Bazel provider-live wrapper does not build `ctx` for skipped or fixture-only
blocker lanes. A true Codex or Pi local-history live run may build or use the
selected `ctx` binary, but the runtime flow invokes only `ctx setup`, `ctx
import`, `ctx search`, `ctx status`, `ctx doctor`, and `ctx validate` with a
scrubbed environment. Provider CLIs, provider API keys, and
provider network credentials are not used by those lane commands.

## Required Evidence For Promotion

Before a provider moves beyond `fixture_only`, `normalized_import_only`, or
`blocked` into native local-history support, the change needs:

- a documented local source format;
- read-only source discovery or an explicit `--path` contract;
- malformed-input tests;
- idempotent re-import tests;
- source citation fields in search output;
- storage and redaction notes for provider-specific sensitive fields;
- a redacted live E2E artifact when claiming live local-history support;
- docs updates in this file and `provider-support-matrix.json`.
