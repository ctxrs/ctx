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
- search indexed sessions and events;
- return JSON for agent-facing workflows;
- keep transcript import and search data local to this machine.

ctx does not run model inference, install shell integration, modify source
repositories, start background processes, require API keys, or use a remote
account for setup, import, or search. First-party analytics and update checks
are configurable separately in `~/.ctx/config.toml`.

## Install Or Run

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

Published install commands are intentionally absent until release artifacts and
verification instructions exist.

## First 10 Minutes

Create local storage and discover provider history:

```bash
ctx setup
ctx status
ctx sources
```

Index local history explicitly:

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
ctx show <item-uuid>
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
ctx status
ctx sources
ctx import
ctx list
ctx show <item-uuid>
ctx search [query]
ctx update
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
ctx show <item-uuid> --json
ctx search [query] --json
ctx update --json
ctx doctor --json
ctx validate --json
```

## Search Data

ctx indexes provider history as sessions and events. An event may be a user
message, assistant message, tool call, command, command output preview, file
reference, lifecycle marker, or provider-specific metadata.

Search results include opaque IDs for `ctx show`, provider names, timestamps,
working-directory metadata when known, snippets, match reasons, and citations.
Raw provider transcript files remain in provider-owned locations such as
`~/.codex/sessions`; ctx stores the searchable text and metadata it needs in
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
- [Production readiness](docs/production-readiness.md)
- [Testing taxonomy](docs/testing-taxonomy.md)
- [Threat model](docs/threat-model.md)
- [Agent usage](docs/agent-usage.md)
- [Troubleshooting](docs/troubleshooting.md)

## Validation

Validation modes are documented in
[Testing taxonomy](docs/testing-taxonomy.md). The default production boundary is
still search-only and local: validation must not imply background collection,
remote account, API-key, or provider-execution behavior.

For docs-only changes, start with:

```bash
bash scripts/check-docs.sh
```

For wider changes, agents should select the smallest documented mode that
answers the question: `fast` for local iteration, `presubmit` before handoff,
`production` for product/security readiness, `release_contract` for
non-publishing release contracts, and `release` only for real artifact proof.

## Design Principles

- Prefer explicit imports over ambient collection.
- Keep raw provider ownership clear.
- Preserve citations so agents can verify retrieved material.
- Keep output deterministic for the same database, query, filters, and limits.
- Treat the local ctx data root as private developer history.
