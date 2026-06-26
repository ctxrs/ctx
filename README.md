# ctx

Search local agent history.

ctx indexes existing local agent transcripts into a local SQLite store so a
future agent can search prior sessions with citations. The first user is an
agent calling the CLI; humans can use the same commands to inspect the index.

## Product Boundary

The current production surface is intentionally narrow:

- discover local provider history locations;
- explicitly import supported local transcripts;
- store a searchable local SQLite index under `~/.ctx` by default;
- search indexed events and return ctx-owned event/session IDs;
- render, locate, and export indexed session transcripts;
- return JSON for agent-facing workflows;
- keep imported transcript text, prompts, and search data in local storage by
  default.

ctx does not run model inference, install shell integration, modify source
repositories, start background processes, require API keys, or use a remote
account for setup, import, or search. No session text, prompts, or transcripts
leave this machine by default.

## Install Or Run

Install the latest stable CLI release:

```bash
curl -fsSL https://cli.ctx.rs/install | sh
```

The install script installs the binary and runs `ctx setup` so discovered local
history is indexed before the command returns. Use
`sh -s -- --no-setup` when you only want to install the binary.

Build from this checkout:

```bash
cargo build -p ctx
cargo install --path crates/ctx-cli
```

Run from source while developing:

```bash
cargo run -p ctx -- status
cargo run -p ctx -- search "retry handling"
```

## First 10 Minutes

Create local storage and index discovered provider history:

```bash
ctx setup
ctx status
ctx sources
```

Re-run or target imports explicitly when you need repair or provider control:

```bash
ctx import --all
ctx import --provider codex
ctx import --provider pi
ctx import --path ~/.codex/sessions
```

Search and inspect results:

```bash
ctx list
ctx search "checkout retry"
ctx show event <ctx-event-id> --window 3
ctx show session <ctx-session-id> --mode lite
ctx locate event <ctx-event-id>
```

Use JSON for agent workflows:

```bash
ctx sources --json
ctx search "sqlite migration" --json
```

## Public CLI

The current command surface is:

```text
ctx setup
ctx setup --catalog-only
ctx status
ctx sources
ctx import
ctx list
ctx search [query]
ctx show session <ctx-session-id>
ctx show event <ctx-event-id>
ctx locate session <ctx-session-id>
ctx locate event <ctx-event-id>
ctx export session <ctx-session-id>
ctx doctor
ctx validate
```

All commands accept the global data-root override:

```bash
ctx --data-root /tmp/ctx status
CTX_DATA_ROOT=/tmp/ctx ctx status
```

Agent-facing commands support `--json` where structured output is useful:

```text
ctx setup --json
ctx status --json
ctx sources --json
ctx import --json
ctx list --json
ctx search [query] --json
ctx show session <ctx-session-id> --format json
ctx show event <ctx-event-id> --format json
ctx locate session <ctx-session-id> --format json
ctx locate event <ctx-event-id> --format json
ctx export session <ctx-session-id> --mode full --format json
ctx doctor --json
ctx validate --json
```

## Search Data

ctx indexes provider history as sessions and events. An event may be a user
message, assistant message, tool call, command, command output preview, file
reference, lifecycle marker, or provider-specific metadata.

Search results are local hits over indexed history. Event hits include
ctx-owned `ctx_event_id`; hits with known session context include
`ctx_session_id`. Results can also include provider names and provider-owned
session IDs as metadata, timestamps, working-directory metadata when known,
source paths/cursors, snippets, match reasons, citations, and suggested next
commands. Raw provider transcript files remain in provider-owned locations such
as `~/.codex/sessions`; ctx stores the searchable text and metadata it needs in
SQLite.

## Docs

- [Product contract](docs/product-contract.md)
- [First 10 minutes](docs/first-10-minutes.md)
- [Getting started](docs/getting-started.md)
- [CLI reference](docs/cli-reference.md)
- [Search](docs/search.md)
- [JSON contracts](docs/contracts/json.md)
- [Storage and privacy](docs/storage.md)
- [Providers](docs/providers.md)
- [Provider support matrix](docs/provider-support.md)
- [Limitations](docs/limitations.md)
- [Security checks](docs/security-checks.md)
- [Testing taxonomy](docs/testing-taxonomy.md)
- [Threat model](docs/threat-model.md)
- [Agent usage](docs/agent-usage.md)
- [Troubleshooting](docs/troubleshooting.md)

## Validation

Validation modes are documented in
[Testing taxonomy](docs/testing-taxonomy.md). The default production boundary is
still local retrieval: validation must not imply background collection, remote
account, API-key, or provider-execution behavior.

For docs-only changes, start with:

```bash
bash scripts/check-docs.sh
```

For wider changes, select the smallest documented mode that answers the
question: `fast` for local iteration, `smoke` for the local CLI flow, and
`presubmit` before handoff.

## Design Principles

- Prefer explicit imports over ambient collection.
- Keep raw provider ownership clear.
- Preserve citations so agents can verify retrieved material.
- Keep output deterministic for the same database, query, filters, and limits.
- Treat the local ctx data root as private developer history.

## License

ctx is licensed under the [Apache License 2.0](LICENSE).
