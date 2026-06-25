# Providers

ctx imports existing agent history through provider adapters. Each adapter must
make a narrow, testable claim about the source format it reads and the event
fields it indexes.

## Supported Local Imports

The current CLI imports local history for:

- Codex session JSONL trees under `~/.codex/sessions`;
- Codex `~/.codex/history.jsonl`;
- Pi `~/.pi/sessions.jsonl` when that local file exists and matches the
  supported JSONL format.

Use `ctx sources` for the truth on the current machine:

```bash
ctx sources
ctx sources --json
```

`ctx sources --json` reports each known provider source with `import_support`
and `native_import` fields. Sources with `import_support: "unsupported"` are
detections or blockers, not importable native history.

## Developer Normalized Inputs

The CLI has a developer/test-only normalized provider JSONL input for Claude,
OpenCode, Antigravity, Gemini, Cursor, Copilot CLI, Factory AI Droid, and Amp.
Set `CTX_PROVIDER_NORMALIZED_IMPORT_DEV=1` when using that input. It is for
adapter harnesses, generated static fixture drafting, and future native importer
development. It is not native provider history discovery or user-facing
provider support.

If one of these providers is selected without a proven native importer, `ctx
import` returns a provider-specific native-history blocker. Do not document one
of these providers as natively locally importable until the CLI can discover or
parse that provider's real local history and the provider support matrix marks
the shipped path accordingly.

## Live Provider E2E

Live provider E2E is opt-in proof, not a provider runner. The lane never
executes provider CLIs, never passes credential environment variables to `ctx`,
and runs `ctx` with a temporary `CTX_DATA_ROOT`.

Only Codex and Pi have native local-history live E2E lanes because those are
the providers with native local import paths in the public CLI. A live run
requires
`CTX_LIVE_PROVIDER_E2E=1`, `CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY=1`, the
provider selector (`CTX_LIVE_PROVIDER_CODEX=1` or `CTX_LIVE_PROVIDER_PI=1`),
an explicit local history path, and a deterministic retrieval query through
`CTX_LIVE_PROVIDER_CODEX_QUERY`, `CTX_LIVE_PROVIDER_PI_QUERY`, or the shared
`CTX_LIVE_PROVIDER_QUERY` fallback:

```bash
CTX_LIVE_PROVIDER_E2E=1 \
CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY=1 \
CTX_LIVE_PROVIDER_CODEX=1 \
CTX_LIVE_PROVIDER_CODEX_SESSIONS_PATH=/path/to/.codex/sessions \
CTX_LIVE_PROVIDER_CODEX_QUERY='private local query' \
scripts/release-provider-live-e2e-lanes.sh run codex

CTX_LIVE_PROVIDER_E2E=1 \
CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY=1 \
CTX_LIVE_PROVIDER_PI=1 \
CTX_LIVE_PROVIDER_PI_SESSIONS_PATH=/path/to/.pi/sessions.jsonl \
CTX_LIVE_PROVIDER_PI_QUERY='private local query' \
scripts/release-provider-live-e2e-lanes.sh run pi
```

The resulting `live-e2e.json` and `live-e2e.md` contain aggregate counts and
booleans only. They must not include raw transcripts, snippets, queries, or
source paths. Provider-specific native lanes for providers without native
local-history importers produce blocked artifacts instead of passing live proof.

OpenRouter-generated histories are a developer drafting aid for static fixtures
only. They are not a provider-live lane, release-contract requirement, CI gate,
or native vendor transcript discovery proof.

Bazel provider-live targets skip the `ctx` build when the lane is skipped or
only writes native-import blocker artifacts. When a real Codex or Pi run is
selected, the wrapper may build or use `ctx`, but the lane runtime still uses
only `ctx` commands with credential and provider CLI environments left out.

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
