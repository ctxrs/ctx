# ctx TypeScript SDK

Experimental in-repo TypeScript/JavaScript client for the `agent-history-v1` ctx API.
The SDK currently talks to a local `ctx` CLI binary and does not require network
access or API keys.

```js
import { createLocalAgentHistoryClient } from "@ctx/agent-history";

const client = createLocalAgentHistoryClient({ dataRoot: "/tmp/ctx" });

await client.init();
const status = await client.status();
const query = {
  version: "ctx-search-v1",
  any: [{ all: "sqlite storage" }],
};
const results = await client.search(query, { refresh: "off" });
```

## API

- `status()` wraps `ctx status --json`.
- `init({ catalogOnly, progress })` wraps `ctx setup --json`.
- `sources()` wraps `ctx sources --json`.
- `import(options)` wraps `ctx import --json`.
- `sync(options)` is an alias for `import(options)`.
- `search(query, options)` and file-based `search(options)` wrap
  `ctx search --query-json <ctx-search-v1>|--file <path> --json`.
- `showEvent(id, { before, after, window })` wraps `ctx show event --format json`.
- `showSession(id, { mode })` wraps `ctx show session --format json`.
- `showSession({ provider, providerSession, mode })` looks up by provider-owned session ID.
- `locateEvent(id)` wraps `ctx locate event --format json`.
- `locateSession(id)` and `locateSession({ provider, providerSession })` wrap `ctx locate session --format json`.
- `version()` wraps `ctx --version` and reports SDK/API version metadata.

All data methods return a `agent-history-v1` envelope with `contractVersion`,
`schemaVersion`, `operation`, and an operation-specific field such as `status`,
`search`, or `location`. TypeScript consumers get operation-specific return
types discriminated by `operation`; CLI JSON remains an adapter detail.

Search uses one canonical structured DTO across local and future hosted adapters:

```ts
import type { SearchQueryV1 } from "@ctx/agent-history";

const query: SearchQueryV1 = {
  version: "ctx-search-v1",
  any: [
    { all: "disk io pressure" },
    { phrase: "storage latency" },
    { literal: "logs_2.db" },
    { semantic: "the indexing job made the workstation sluggish" },
  ],
  must: [{ all: "codex" }],
  must_not: [{ all: "postgres vacuum" }],
};

const response = await client.search(query, {
  provider: "custom",
  historySource: "dorkos/default",
  providerKey: "dorkos",
  sourceId: "default",
  sourceFormat: "dorkos-history-v1",
  backend: "hybrid",
  limit: 20,
  refresh: "off",
});
```

`any` clauses are alternatives, every `must` clause is required, and any
`must_not` match excludes the candidate. A semantic clause is allowed only once
and only in `any`. Validation collapses whitespace for non-literal clauses,
trims literals, removes duplicate clauses within each placement, and enforces
1 to 32 analyzed tokens per clause. Search limits must be integers from 1 to
200. `historySource`, `providerKey`, `sourceId`, and `sourceFormat` map to the
matching CLI and MCP source-identity filters; `provider` includes every CLI
provider value, including `custom`.

Search payloads expose `schema_version: 2`, the canonical query, and
`query_execution` with resolved and consumed work budgets, truncation reasons,
semantic readiness, coverage, and completeness.

## Dogfood Example

```bash
node sdks/typescript/examples/dogfood-toy.js
```

The example runs `status`, `search`, `show event`, `show session`,
`locate event`, and `locate session` against a mocked local runner by default.
Set `CTX_SDK_EXAMPLE_CTX_PATH` to point it at a real `ctx` binary instead.

## Local CLI Adapter

```js
import { LocalCliAdapter, LocalAgentHistoryClient } from "@ctx/agent-history";

const adapter = new LocalCliAdapter({
  ctxPath: "ctx",
  dataRoot: "/tmp/ctx",
  timeoutMs: 60_000,
});

const client = new LocalAgentHistoryClient({ adapter });
```

For tests, pass a `runner` function to `LocalCliAdapter` or
`createLocalAgentHistoryClient`. The runner receives `{ command, args, cwd, env,
timeoutMs }` and returns `{ exitCode, stdout, stderr }`.

## Hosted Placeholder

`createHostedAgentHistoryClient()` and `createAgentHistoryClient({ hosted: true })` reserve
the future hosted transport shape. Any data method rejects with
`CtxUnsupportedError` until ctx exposes a hosted agent-history-v1 service.

## Errors

- `CtxCliError` includes `exitCode`, `signal`, `stdout`, `stderr`, `command`,
  and `args`.
- `CtxParseError` is raised when a JSON CLI command returns invalid JSON.
- `CtxValidationError` is raised before invoking the CLI for invalid SDK input.
- `CtxUnsupportedError` is raised by the hosted placeholder.

## Development

```bash
npm install --prefix sdks/typescript
npm test --prefix sdks/typescript
```

Tests use Node's built-in test runner, mocked CLI runners, the dogfood example,
shared `contracts/agent-history-v1/fixtures`, and a strict handwritten declaration
typecheck.
