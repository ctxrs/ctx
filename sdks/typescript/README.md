# ctx TypeScript SDK

Experimental in-repo TypeScript/JavaScript client for the `agent-history-v1` ctx API.
The SDK currently talks to a local `ctx` CLI binary and does not require network
access or API keys.

```js
import { createLocalAgentHistoryClient } from "@ctx/agent-history";

const client = createLocalAgentHistoryClient({ dataRoot: "/tmp/ctx" });

await client.init();
const status = await client.status();
const results = await client.search("sqlite storage", { refresh: "off" });
```

## API

- `status()` wraps `ctx status --json`.
- `init({ catalogOnly, progress })` wraps `ctx setup --json`.
- `sources()` wraps `ctx sources --json`.
- `import(options)` wraps `ctx import --json`.
- `sync(options)` is an alias for `import(options)`.
- `search(query, options)` and file/term-based `search(options)` wrap
  `ctx search --json`.
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
