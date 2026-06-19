# Harness Starter Boundaries

This example records the intended module shape for a future local harness
starter. It is a design hook, not an implementation contract.

The harness speaks ACP to ctx. ctx-owned helpers are optional modules behind
that ACP boundary.

```text
examples/harness-starter/
  package.json
  plugin.json
  src/
    agent/
      loop.ts
      model.ts
      tools.ts
    acp/
      server.ts
      sessions.ts
      commands.ts
    ctx/
      shell.ts
      files.ts
      edits.ts
      sandbox.ts
      transcript.ts
      artifacts.ts
      work-capture.ts
```

## Boundary Rules

- `src/acp/*` owns the ACP adapter and JSON-RPC stdio server.
- `src/agent/*` owns agent-loop policy, model routing, prompts, and tool choice.
- `src/ctx/*` contains optional adapters to ctx local primitives.
- `plugin.json` registers the ACP process as a provider contribution.
- No module writes directly to the Work store.
- No module requires a ctx-specific agent protocol.

## Example Package Split

Future packages may use this split:

| Package | Depends on | Does not depend on |
| --- | --- | --- |
| `@ctx/harness-core` | TypeScript runtime types | ctx daemon runtime, Workbench UI |
| `@ctx/harness-acp` | ACP schemas, JSON-RPC stdio helpers | Work store internals |
| `@ctx/harness-shell` | ctx execution/sandbox request types | agent-loop policy |
| `@ctx/harness-files` | workspace file request types | provider registry |
| `@ctx/harness-edits` | patch/change-set helper types | Workbench templates |
| `@ctx/harness-transcript` | Work event payload builders | direct store writes |
| `@ctx/harness-artifacts` | artifact metadata/redaction helpers | remote artifact services |
| `@ctx/harness-work-capture` | approved local capture/import client | database handles |
| `@ctx/harness-plugin` | plugin manifest helper types | agent-loop implementation |

Package names may change. The important decision is the dependency direction:
agent code calls optional adapters, adapters submit approved ACP or Work-capture
payloads, and ctx receives an ACP provider contribution.

## Minimal Manifest Shape

```json
{
  "id": "example.harness-provider",
  "name": "Example Harness Provider",
  "version": "0.1.0",
  "entrypoints": [
    {
      "id": "harness",
      "kind": "process",
      "command": "node",
      "args": ["./dist/acp/server.js"]
    }
  ],
  "contributes": {
    "providers": [
      {
        "id": "example-harness",
        "name": "Example Harness",
        "entrypoint": "harness",
        "capabilities": ["acp.v1"]
      }
    ]
  }
}
```

## Implementation Hooks

The first implementation should be able to land incrementally:

1. Add an ACP stdio server fixture with no ctx helper dependencies.
2. Add Work transcript and tool-call capture helpers as pure payload builders.
3. Add optional shell/sandbox/file/edit adapters through approved ctx actions.
4. Add artifact helpers with local redaction defaults.
5. Add plugin manifest helpers only after provider plugin schema support exists.

Each step should keep ACP conformance tests as the public compatibility target.
