import assert from "node:assert/strict";
import { mkdtemp, readdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  CtxCliError,
  CtxParseError,
  CtxTimeoutError,
  CtxUnsupportedError,
  CtxValidationError,
  AGENT_HISTORY_V1_VERSION,
  LocalCliAdapter,
  createHostedAgentHistoryClient,
  createLocalAgentHistoryClient,
  toAgentHistoryEnvelope,
} from "../src/index.js";
import { runDogfoodToy } from "../examples/dogfood-toy.js";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const subprocessFixture = fileURLToPath(
  new URL("./fixtures/local-cli-subprocess-lane.mjs", import.meta.url),
);
const windowsJobLauncher = fileURLToPath(
  new URL("../src/windows-job-launcher.ps1", import.meta.url),
);

function mockClient(handler) {
  const calls = [];
  const client = createLocalAgentHistoryClient({
    dataRoot: "/tmp/ctx-sdk-test",
    runner: async (request) => {
      calls.push(request);
      return handler(request);
    },
  });
  return { client, calls };
}

test("wraps status, init, sources, import, and sync CLI commands", async () => {
  const { client, calls } = mockClient(({ args }) => ({
    stdout: JSON.stringify({ initialized: true, sources: [{ provider: "codex" }], args }),
  }));

  const status = await client.status();
  await client.init();
  const sources = await client.sources();
  const imported = await client.import({ provider: "codex", resume: true });
  await client.sync({ all: true });

  assert.equal(status.contractVersion, AGENT_HISTORY_V1_VERSION);
  assert.equal(status.operation, "status");
  assert.equal(status.status.initialized, true);
  assert.equal(sources.sources[0].provider, "codex");
  assert.equal(imported.operation, "import");

  assert.deepEqual(
    calls.map((call) => call.args),
    [
      ["--data-root", "/tmp/ctx-sdk-test", "status", "--format=json"],
      [
        "--data-root",
        "/tmp/ctx-sdk-test",
        "setup",
        "--format=json",
        "--progress",
        "none",
      ],
      ["--data-root", "/tmp/ctx-sdk-test", "sources", "--format=json"],
      [
        "--data-root",
        "/tmp/ctx-sdk-test",
        "import",
        "--format=json",
        "--progress",
        "none",
        "--provider",
        "codex",
        "--resume",
      ],
      [
        "--data-root",
        "/tmp/ctx-sdk-test",
        "import",
        "--format=json",
        "--progress",
        "none",
        "--all",
      ],
    ],
  );
});

test("status counters use the exact cross-SDK integer domain", async () => {
  const maximum = Number.MAX_SAFE_INTEGER;
  const accepted = toAgentHistoryEnvelope("status", {
    initialized: true,
    indexed_items: maximum,
    indexed_sessions: maximum,
    indexed_events: maximum,
    indexed_sources: maximum,
  });
  assert.equal(accepted.status.indexedItems, maximum);
  assert.equal(accepted.status.indexedSessions, maximum);
  assert.equal(accepted.status.indexedEvents, maximum);
  assert.equal(accepted.status.indexedSources, maximum);

  for (const wireValue of ["9007199254740993", "18446744073709551615"]) {
    const raw = JSON.parse(`{"initialized":true,"indexed_items":${wireValue}}`);
    assert.throws(
      () => toAgentHistoryEnvelope("status", raw),
      (error) =>
        error instanceof CtxParseError &&
        error.details.field === "indexedItems" &&
        error.details.maximum === maximum,
    );
  }

  for (const wireValue of ["1.00000000000000001", "1e0"]) {
    const { client } = mockClient(() => ({
      stdout: `{"initialized":true,"indexed_items":${wireValue}}`,
    }));
    await assert.rejects(
      () => client.status(),
      (error) =>
        error instanceof CtxParseError &&
        error.details.field === "indexedItems" &&
        error.details.maximum === maximum,
    );
  }
});

test("status counter lexeme checks do not reinterpret other operation payloads", async () => {
  const { client } = mockClient(() => ({
    stdout: '{"results":[],"indexed_items":1.5}',
  }));
  const response = await client.search("needle");
  assert.equal(response.search.indexedItems, 1.5);
});

test("status counter lexeme checks ignore extension fields below the status root", async () => {
  const { client } = mockClient(() => ({
    stdout: '{"initialized":true,"daemon":{"indexed_items":1.5}}',
  }));
  const response = await client.status();
  assert.equal(response.status.daemon.indexedItems, 1.5);
});

test("forces analytics off after ambient and user environment merging", async () => {
  const original = process.env.CTX_ANALYTICS_ENABLED;
  process.env.CTX_ANALYTICS_ENABLED = "true";
  try {
    const adapter = new LocalCliAdapter({
      ctxPath: process.execPath,
      env: { CTX_ANALYTICS_ENABLED: "true" },
    });
    const result = await adapter.execute(
      ["-e", "process.stdout.write(process.env.CTX_ANALYTICS_ENABLED ?? '')"],
      { env: { CTX_ANALYTICS_ENABLED: "true" } },
    );

    assert.equal(result.exitCode, 0);
    assert.equal(result.stdout, "false");
  } finally {
    if (original === undefined) {
      delete process.env.CTX_ANALYTICS_ENABLED;
    } else {
      process.env.CTX_ANALYTICS_ENABLED = original;
    }
  }
});

test("builds search flags and normalizes nested CLI search output", async () => {
  const { client, calls } = mockClient(() =>
    JSON.stringify({
      query: "retry handling",
      generated_at: "2026-07-01T12:00:00Z",
      freshness: { mode: "off", status: "skipped", source_count: 1, totals: {} },
      retrieval: {
        requested_mode: "hybrid",
        effective_mode: "lexical",
        semantic_weight: 0.0,
        semantic_status: "fallback",
        semantic_fallback_code: "semantic_retrieval_failed",
        semantic_fallback: "semantic_retrieval_failed",
        coverage: {
          embedded_items: 4,
          embedded_chunks: 9,
          searchable_items: 12,
          indexed_now: 1,
        },
        diagnostics: { query_embed_ms: 2, vector_scan_ms: 3 },
      },
      results: [
        {
          ctx_event_id: "00000000-0000-0000-0000-000000000101",
          ctx_session_id: "00000000-0000-0000-0000-000000000102",
          provider_session_id: "codex-session",
          provider: "codex",
          source_format: "codex_session_jsonl",
          event_seq: 7,
          result_type: "event",
          result_scope: "event",
          rank: 1,
          retrieval_score: 0.98,
          why_matched: ["text"],
          citations: [
            {
              target_type: "event",
              ctx_event_id: "00000000-0000-0000-0000-000000000101",
              ctx_session_id: "00000000-0000-0000-0000-000000000102",
            },
          ],
        },
      ],
      result_window: { limit: 1, returned: 1, more_available: true },
      truncation: { truncated: false },
    }),
  );

  const result = await client.search("retry handling", {
    terms: ["timeout", "backoff"],
    limit: 5,
    provider: "codex",
    workspace: "ctx",
    since: "30d",
    primaryOnly: true,
    eventType: "message",
    file: "crates/foo/src/lib.rs",
    session: "00000000-0000-0000-0000-000000000001",
    events: true,
    backend: "hybrid",
    semanticWeight: 0.8,
    refresh: "off",
    includeCurrentSession: true,
  });

  assert.equal(result.contractVersion, AGENT_HISTORY_V1_VERSION);
  assert.equal(result.operation, "search");
  assert.equal(result.search.generatedAt, "2026-07-01T12:00:00Z");
  assert.equal(result.search.freshness.sourceCount, 1);
  assert.equal(result.search.results[0].ctxEventId, "00000000-0000-0000-0000-000000000101");
  assert.equal(result.search.results[0].ctxSessionId, "00000000-0000-0000-0000-000000000102");
  assert.equal(result.search.results[0].providerSessionId, "codex-session");
  assert.equal(result.search.results[0].provider, "codex");
  assert.equal(result.search.results[0].sourceFormat, "codex_session_jsonl");
  assert.equal(result.search.results[0].eventSeq, 7);
  assert.equal(result.search.results[0].resultType, "event");
  assert.equal(result.search.results[0].resultScope, "event");
  assert.equal(result.search.results[0].rank, 1);
  assert.equal(result.search.results[0].retrievalScore, 0.98);
  assert.equal(result.search.results[0].whyMatched[0], "text");
  assert.equal(result.search.results[0].citations[0].targetType, "event");
  assert.equal(result.search.retrieval.requestedMode, "hybrid");
  assert.equal(result.search.retrieval.effectiveMode, "lexical");
  assert.equal(result.search.retrieval.semanticWeight, 0.0);
  assert.equal(result.search.retrieval.semanticFallbackCode, "semantic_retrieval_failed");
  assert.equal(result.search.retrieval.semanticFallback, "semantic_retrieval_failed");
  assert.equal(result.search.retrieval.coverage.embeddedItems, 4);
  assert.equal(result.search.retrieval.coverage.indexedNow, 1);
  assert.equal(result.search.retrieval.diagnostics.queryEmbedMs, 2);
  assert.deepEqual(result.search.resultWindow, {
    limit: 1,
    returned: 1,
    moreAvailable: true,
  });
  assert.equal(result.search.pagination.limit, 1);
  assert.equal(result.search.pagination.hasMore, true);
  assert.equal(result.search.pagination.nextCursor, undefined);

  assert.deepEqual(calls[0].args, [
    "--data-root",
    "/tmp/ctx-sdk-test",
    "search",
    "retry handling",
    "--term",
    "timeout",
    "--term",
    "backoff",
    "--limit",
    "5",
    "--provider",
    "codex",
    "--workspace",
    "ctx",
    "--since",
    "30d",
    "--primary-only",
    "--event-type",
    "message",
    "--file",
    "crates/foo/src/lib.rs",
    "--session",
    "00000000-0000-0000-0000-000000000001",
    "--events",
    "--backend",
    "hybrid",
    "--semantic-weight",
    "0.8",
    "--refresh",
    "off",
    "--include-current-session",
    "--format=json",
  ]);
});

test("omits semantic search override flags when unset", async () => {
  const { client, calls } = mockClient(() => JSON.stringify({ query: "default", results: [] }));

  await client.search("default");

  assert.equal(calls[0].args.includes("--backend"), false);
  assert.equal(calls[0].args.includes("--semantic-weight"), false);
  assert.equal(calls[0].args.includes("--content-scope"), false);
});

test("forwards exactly one class-aware search content scope", async () => {
  const { client, calls } = mockClient(() => JSON.stringify({ query: "tool calls", results: [] }));

  await client.search("tool calls", { contentScope: "calls" });

  const args = calls[0].args;
  assert.equal(args.filter((arg) => arg === "--content-scope").length, 1);
  assert.equal(args[args.indexOf("--content-scope") + 1], "calls");
});

test("rejects content scope with event type before invoking CLI", async () => {
  const { client, calls } = mockClient(() => {
    throw new Error("runner should not be called");
  });

  await assert.rejects(
    () => client.search("messages", { contentScope: "all", eventType: "message" }),
    (error) =>
      error instanceof CtxValidationError &&
      error.code === "CTX_VALIDATION_ERROR" &&
      error.message === "search contentScope and eventType are mutually exclusive" &&
      error.details.contentScope === "all" &&
      error.details.eventType === "message",
  );
  assert.equal(calls.length, 0);
});

test("rejects an invalid content scope before invoking CLI", async () => {
  const { client, calls } = mockClient(() => {
    throw new Error("runner should not be called");
  });

  for (const contentScope of ["messages", "All", "outputs ", 1, {}]) {
    await assert.rejects(
      () => client.search("messages", { contentScope }),
      (error) =>
        error instanceof CtxValidationError &&
        error.code === "CTX_VALIDATION_ERROR" &&
        error.message ===
          "search contentScope must be one of all, transcript, calls, outputs" &&
        error.details.contentScope === contentScope,
    );
  }
  assert.equal(calls.length, 0);
});

test("rejects search without query, term, or file before invoking CLI", async () => {
  const { client, calls } = mockClient(() => {
    throw new Error("runner should not be called");
  });

  await assert.rejects(() => client.search(), CtxValidationError);
  await assert.rejects(() => client.search({ refresh: "off", limit: 5 }), CtxValidationError);
  await assert.rejects(() => client.search("   "), CtxValidationError);

  assert.equal(calls.length, 0);
});

test("wraps show commands by ctx id and provider session id", async () => {
  const { client, calls } = mockClient(() => "{}");

  await client.showEvent("00000000-0000-0000-0000-000000000002", { window: 3 });
  await client.showSession("00000000-0000-0000-0000-000000000003", { mode: "full" });
  await client.showSession({ provider: "codex", providerSession: "codex-session", mode: "log" });

  assert.deepEqual(
    calls.map((call) => call.args.slice(2)),
    [
      [
        "show",
        "event",
        "00000000-0000-0000-0000-000000000002",
        "--format",
        "json",
        "--window",
        "3",
      ],
      [
        "show",
        "session",
        "00000000-0000-0000-0000-000000000003",
        "--mode",
        "full",
        "--format",
        "json",
      ],
      [
        "show",
        "session",
        "--provider",
        "codex",
        "--provider-session",
        "codex-session",
        "--mode",
        "log",
        "--format",
        "json",
      ],
    ],
  );
});

test("normalizes typed Core show metadata", () => {
  const event = toAgentHistoryEnvelope("showEvent", {
    event: {
      ctx_event_id: "event-1",
      ctx_session_id: "session-1",
      provider: "codex",
      provider_session_id: "codex-resume-uuid",
      source_format: "codex_session_jsonl",
      text: "complete body",
      content: {
        complete: true,
        policy_status: "selected",
      },
    },
    events: [],
  });
  assert.equal(event.event.event.providerSessionId, "codex-resume-uuid");
  assert.equal(event.event.event.sourceFormat, "codex_session_jsonl");
  assert.deepEqual(event.event.event.content, {
    complete: true,
    policyStatus: "selected",
  });

  const session = toAgentHistoryEnvelope("showSession", {
    session: {
      ctx_session_id: "session-1",
      provider: "codex",
      provider_session_id: "codex-resume-uuid",
      source_format: "codex_session_jsonl",
    },
    events: [],
  });
  assert.equal(session.session.session.providerSessionId, "codex-resume-uuid");
  assert.equal(session.session.session.sourceFormat, "codex_session_jsonl");
});

test("normalizes an exact bounded MCP tool-call object while retaining outer additions", () => {
  const envelope = toAgentHistoryEnvelope("showEvent", {
    event: {
      ctx_event_id: "event-1",
      mcp_tool_call: {
        server: "mcp-サーバー-🦀",
        tool: "検索/工具/🛠️",
      },
      future_event_field: { preserved: true },
    },
    events: [{ ctx_event_id: "event-2" }],
  });

  assert.deepEqual(envelope.event.event.mcpToolCall, {
    server: "mcp-サーバー-🦀",
    tool: "検索/工具/🛠️",
  });
  assert.deepEqual(envelope.event.event.futureEventField, { preserved: true });
  assert.equal("mcpToolCall" in envelope.event.events[0], false);
  assert.deepEqual(
    JSON.parse(JSON.stringify(envelope.event.event.mcpToolCall)),
    envelope.event.event.mcpToolCall,
  );

  const exact = toAgentHistoryEnvelope("showEvent", {
    event: { mcp_tool_call: { server: " ", tool: "🦀".repeat(16_384) } },
    events: [],
  });
  assert.equal(new TextEncoder().encode(exact.event.event.mcpToolCall.tool).byteLength, 64 * 1024);

  for (const invalid of [
    { server: "server" },
    { server: "server", tool: "tool", futureLabel: true },
    { server: "", tool: "tool" },
    { server: "server", tool: "a".repeat(64 * 1024 + 1) },
    { server: "server", tool: 7 },
    { server: "server", tool: "\ud800" },
    null,
  ]) {
    assert.throws(
      () => toAgentHistoryEnvelope("showEvent", { event: { mcpToolCall: invalid }, events: [] }),
      CtxParseError,
    );
  }
});

test("exposes a typed lossless MCP exchange without normalizing captured JSON keys", async () => {
  const fixture = JSON.parse(
    await readFile(
      join(repoRoot, "contracts", "agent-history-v1", "fixtures", "show-event.mcp-tool-call.json"),
      "utf8",
    ),
  );
  const envelope = toAgentHistoryEnvelope("showEvent", fixture.event);
  const exchange = envelope.event.event.mcpExchange;
  assert.equal(exchange.providerCallId, "native-call-呼び出し-🦀");
  assert.equal(exchange.response.durationNs, Number.MAX_SAFE_INTEGER);
  assert.deepEqual(exchange.invocation.arguments.value, {
    snake_key: ["雪", null, { camelKey: true }],
    nested: { items: [1, { deep_null: null }] },
  });
  assert.equal(Object.hasOwn(exchange.invocation.arguments.value, "snake_key"), true);
  assert.equal(Object.hasOwn(exchange.invocation.arguments.value, "snakeKey"), false);
  assert.equal(envelope.event.events[2].mcpExchange.response.text.observedEncodedBytes, Number.MAX_SAFE_INTEGER);
  assert.equal("mcpExchange" in envelope.event.events[3], false);

  const raw = toAgentHistoryEnvelope("showEvent", {
    event: {
      text: "body",
      mcp_exchange: {
        provider_call_id: "call",
        invocation: {
          server: "server",
          tool: "tool",
          arguments: {
            capture_status: "present",
            value: { snake_key: { deep_null: null }, camelKey: [1, false] },
          },
        },
        response: {
          status: "succeeded",
          duration_ns: Number.MAX_SAFE_INTEGER,
          text: { capture_status: "normalized_body" },
          payload: {
            capture_status: "present",
            value: { result_key: ["雪", null] },
          },
        },
      },
    },
    events: [],
  });
  assert.deepEqual(raw.event.event.mcpExchange.response.payload.value, {
    result_key: ["雪", null],
  });
});

test("rejects raw MCP duplicates without matching repeated string contents", async () => {
  const fixtureDir = join(repoRoot, "contracts", "agent-history-v1", "fixtures", "adversarial");
  for (const name of [
    "duplicate-event-mcp-tool-call-snake.json",
    "duplicate-event-mcp-tool-call-camel.json",
    "duplicate-mcp-tool-call-server.json",
    "duplicate-mcp-tool-call-tool.json",
    "duplicate-event-mcp-exchange-snake.json",
    "duplicate-mcp-exchange-captured-value.json",
    "invalid-mcp-exchange-explicit-null.json",
    "invalid-mcp-exchange-outer-alias-collision.json",
    "invalid-mcp-exchange-unknown-field.json",
    "invalid-mcp-exchange-normalized-body-missing-event-text.json",
    "invalid-mcp-exchange-normalized-body-empty-event-text.json",
    "invalid-mcp-exchange-unsafe-duration-ns.json",
    "invalid-mcp-exchange-unsafe-observed-encoded-bytes.json",
    "invalid-mcp-tool-call-transformed-server.json",
    "invalid-mcp-tool-call-transformed-tool.json",
    "invalid-mcp-tool-call-transformed-collision.json",
    "invalid-mcp-tool-call-outer-alias-collision.json",
    "invalid-mcp-tool-call-outer-mixed-case.json",
    "invalid-mcp-tool-call-outer-repeated-separator.json",
    "invalid-mcp-tool-call-outer-trailing-separator.json",
    "invalid-mcp-tool-call-outer-camel-snake.json",
  ]) {
    const bytes = await readFile(join(fixtureDir, name));
    const { client } = mockClient(() => ({ stdout: bytes }));
    await assert.rejects(() => client.showEvent("event-1"), CtxParseError, name);
  }

  const repeated = await readFile(join(fixtureDir, "valid-repeated-string-contents.json"));
  const { client } = mockClient(() => ({ stdout: repeated }));
  const response = await client.showEvent("event-1");
  assert.deepEqual(response.event.event.mcpToolCall, {
    server: "server server",
    tool: "tool tool",
  });

  const aliases = await readFile(join(fixtureDir, "valid-mcp-tool-call-outer-aliases.json"));
  const aliasClient = mockClient(() => ({ stdout: aliases })).client;
  const aliasResponse = await aliasClient.showEvent("event-1");
  assert.equal(aliasResponse.event.event.mcpToolCall.server, "snake-server");
  assert.deepEqual(aliasResponse.event.event.mcpToolCalls, { note: "ordinary unknown" });
  assert.equal(aliasResponse.event.event.futureEventField, "snake-extra");
  assert.equal(aliasResponse.event.events[0].mcpToolCall.server, "camel-server");
  assert.deepEqual(aliasResponse.event.events[0].mcpToolCalls, { note: "ordinary unknown" });
  assert.equal(aliasResponse.event.events[0].futureEventField, "camel-extra");
});

test("rejects invalid UTF-8 bytes before JSON parsing but accepts encoded U+FFFD", async () => {
  const prefix = Buffer.from('{"event":{"mcp_tool_call":{"server":"');
  const suffix = Buffer.from('","tool":"tool"}},"events":[]}');
  const invalid = Buffer.concat([prefix, Buffer.from([0xff]), suffix]);
  const invalidClient = mockClient(() => ({ stdout: invalid })).client;
  await assert.rejects(
    () => invalidClient.showEvent("event-1"),
    (error) => error instanceof CtxParseError && /invalid UTF-8/u.test(error.message),
  );

  const replacement = Buffer.from(
    '{"event":{"mcp_tool_call":{"server":"�","tool":"tool"}},"events":[]}',
    "utf8",
  );
  const validClient = mockClient(() => ({ stdout: replacement })).client;
  const response = await validClient.showEvent("event-1");
  assert.equal(response.event.event.mcpToolCall.server, "�");
});

test("preserves legitimate source semantics in sources and import payloads", () => {
  const acquisition = {
    source: "local_scan",
    cursor: "opaque-checkpoint",
  };
  const sources = toAgentHistoryEnvelope("sources", {
    sources: [{ provider: "codex", path: "/configured/root", acquisition }],
  });
  assert.deepEqual(sources.sources[0].acquisition, acquisition);

  const imported = toAgentHistoryEnvelope("import", {
    resume: false,
    totals: {},
    sources: [{ source: acquisition }],
  });
  assert.deepEqual(imported.import.sources[0].source, acquisition);
});

test("reports versioning metadata", async () => {
  const { client } = mockClient(() => "ctx 1.2.3\n");

  assert.deepEqual(await client.version(), {
    schema_version: 1,
    api_version: AGENT_HISTORY_V1_VERSION,
    sdk_version: "0.0.0",
    adapter: "local-cli",
    ctx_version: "1.2.3",
  });
});

test("raises structured errors", async () => {
  const cli = createLocalAgentHistoryClient({
    runner: () => ({ exitCode: 2, stderr: "bad flag\n" }),
  });
  await assert.rejects(
    () => cli.status(),
    (error) => {
      assert.ok(error instanceof CtxCliError);
      assert.equal(error.message, "ctx status --format=json failed");
      assert.deepEqual(error.details, {
        command: "ctx",
        args: ["status", "--format=json"],
        exitCode: 2,
        signal: undefined,
        stdout: "",
        stderr: "bad flag\n",
      });
      return true;
    },
  );

  const parse = createLocalAgentHistoryClient({ runner: () => "not json" });
  await assert.rejects(
    () => parse.status(),
    (error) => {
      assert.ok(error instanceof CtxParseError);
      assert.equal(error.message, "ctx returned invalid JSON: trailing data");
      assert.deepEqual(error.details, { offset: 4 });
      return true;
    },
  );

  await assert.rejects(() => parse.showEvent(""), CtxValidationError);
  await assert.rejects(() => parse.showSession({ provider: "codex" }), CtxValidationError);
});

test("raises timeout errors from the local adapter", async () => {
  const adapter = new (await import("../src/index.js")).LocalCliAdapter({
    ctxPath: process.execPath,
    timeoutMs: 1,
  });
  await assert.rejects(
    () => adapter.execute(["-e", "setTimeout(() => {}, 1000)"]),
    CtxTimeoutError,
  );
});

test("local adapter preserves structured spawn errors without waiting for close", async () => {
  const command = join(tmpdir(), `ctx-ts-missing-${process.pid}-${Date.now()}`);
  const adapter = new LocalCliAdapter({ ctxPath: command, timeoutMs: 2_000 });

  await assert.rejects(
    () => adapter.execute(["status"]),
    (error) => {
      assert.ok(error instanceof CtxCliError);
      assert.equal(error.message, `failed to start ${command}`);
      assert.deepEqual(error.details, {
        command,
        args: ["status"],
        exitCode: undefined,
        signal: undefined,
        stdout: "",
        stderr: "",
      });
      return true;
    },
  );
});

test("local adapter preserves exact successful output while draining both pipes", async () => {
  const adapter = new LocalCliAdapter({ ctxPath: process.execPath, timeoutMs: 2_000 });
  const result = await adapter.execute([subprocessFixture, "exact-json"]);

  assert.equal(result.stdout, ' \n{"message":"exact 日本語 🦀","nested":[1,true,null]}\n');
  assert.equal(result.stderr, "exact stderr\n");
  assert.equal(result.exitCode, 0);
  assert.equal(result.signal, null);
});

test("local adapter terminates a persistent descendant that holds output pipes", async () => {
  const directory = await mkdtemp(join(tmpdir(), "ctx-ts-persistent-pipe-"));
  const pidPath = join(directory, "descendant.pid");
  let descendantPid;
  try {
    const adapter = new LocalCliAdapter({ ctxPath: process.execPath, timeoutMs: 2_000 });
    const result = await adapter.execute([subprocessFixture, "persistent-pipe", pidPath]);
    descendantPid = Number(await readFile(pidPath, "utf8"));

    assert.equal(result.stdout, ' \n{"message":"exact 日本語 🦀","nested":[1,true,null]}\n');
    await assertProcessExited(descendantPid);
    descendantPid = undefined;
  } finally {
    if (descendantPid) await forceProcessExit(descendantPid);
    await rm(directory, { recursive: true, force: true });
  }
});

test("local adapter owns a silent descendant after the root PID and pipes are gone", async () => {
  const directory = await mkdtemp(join(tmpdir(), "ctx-ts-persistent-silent-"));
  const pidPath = join(directory, "descendant.pid");
  let descendantPid;
  try {
    const adapter = new LocalCliAdapter({ ctxPath: process.execPath, timeoutMs: 2_000 });
    const result = await adapter.execute([subprocessFixture, "persistent-silent", pidPath]);
    descendantPid = Number(await readFile(pidPath, "utf8"));

    assert.equal(result.stdout, ' \n{"message":"exact 日本語 🦀","nested":[1,true,null]}\n');
    await assertProcessExited(descendantPid);
    descendantPid = undefined;
  } finally {
    if (descendantPid) await forceProcessExit(descendantPid);
    await rm(directory, { recursive: true, force: true });
  }
});

test("Windows launcher uses atomic kill-on-close Job ownership", async () => {
  const source = await readFile(windowsJobLauncher, "utf8");

  assert.match(source, /PROC_THREAD_ATTRIBUTE_JOB_LIST/);
  assert.match(source, /JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE/);
  assert.match(source, /EXTENDED_STARTUPINFO_PRESENT/);
  assert.match(source, /UpdateProcThreadAttribute\(job list\)/);
  assert.ok(
    source.indexOf("UpdateProcThreadAttribute(job list)") <
      source.indexOf("if (!CreateProcess("),
    "the Job list must be installed before atomic process creation",
  );
  assert.doesNotMatch(source, /CREATE_SUSPENDED/);
  assert.doesNotMatch(source, /\btaskkill\b/i);
});

test("local adapter deadline remains active after process exit until inherited pipes end", async () => {
  const directory = await mkdtemp(join(tmpdir(), "ctx-ts-pipe-deadline-"));
  const pidPath = join(directory, "descendant.pid");
  let descendantPid;
  const timeoutMs = 1_000;
  try {
    const adapter = new LocalCliAdapter({ ctxPath: process.execPath, timeoutMs });
    await assert.rejects(
      () =>
        adapter.execute([
          subprocessFixture,
          "persistent-pipe-ignored-term",
          pidPath,
          "500",
        ]),
      (error) => {
        assert.ok(error instanceof CtxTimeoutError);
        assert.equal(error.message, `ctx command timed out after ${timeoutMs}ms`);
        assert.equal(error.details.exitCode, 0);
        assert.equal(error.details.signal, null);
        assert.equal(
          error.details.stdout,
          ' \n{"message":"exact 日本語 🦀","nested":[1,true,null]}\n',
        );
        return true;
      },
    );
    descendantPid = Number(await readFile(pidPath, "utf8"));
    await assertProcessExited(descendantPid);
    descendantPid = undefined;
  } finally {
    if (descendantPid) await forceProcessExit(descendantPid);
    await rm(directory, { recursive: true, force: true });
  }
});

test("local adapter force-kills a TERM-resistant process tree after one deadline", async () => {
  const directory = await mkdtemp(join(tmpdir(), "ctx-ts-ignored-term-"));
  const pidPath = join(directory, "tree.pids.json");
  let pids = [];
  const timeoutMs = 500;
  const started = performance.now();
  try {
    const adapter = new LocalCliAdapter({ ctxPath: process.execPath, timeoutMs });
    await assert.rejects(
      () => adapter.execute([subprocessFixture, "ignored-term", pidPath]),
      (error) => {
        assert.ok(error instanceof CtxTimeoutError);
        assert.equal(error.message, `ctx command timed out after ${timeoutMs}ms`);
        assert.equal(error.details.timeoutMs, timeoutMs);
        assert.equal(
          error.details.stdout,
          ' \n{"message":"exact 日本語 🦀","nested":[1,true,null]}\n',
        );
        return true;
      },
    );
    assert.ok(performance.now() - started < 2_000, "TERM-resistant teardown stayed bounded");
    pids = JSON.parse(await readFile(pidPath, "utf8"));
    for (const pid of pids) await assertProcessExited(pid);
    pids = [];
  } finally {
    for (const pid of pids) await forceProcessExit(pid);
    await rm(directory, { recursive: true, force: true });
  }
});

test("local adapter accepts valid JSON above the former 2 MiB stdout cap", async () => {
  const adapter = new LocalCliAdapter({ ctxPath: process.execPath, timeoutMs: 10_000 });
  const result = await adapter.execute([subprocessFixture, "large-valid-json"]);
  const parsed = JSON.parse(result.stdout);

  assert.ok(Buffer.byteLength(result.stdout) > 2 * 1024 * 1024);
  assert.equal(parsed.payload.length, 3 * 1024 * 1024);
  assert.equal(result.exitCode, 0);
});

test("local adapter bounds retained stdout and stderr while continuing to drain", async () => {
  const adapter = new LocalCliAdapter({ ctxPath: process.execPath, timeoutMs: 15_000 });
  for (const [mode, stream, capBytes] of [
    ["oversized-stdout", "stdout", 64 * 1024 * 1024],
    ["oversized-stderr", "stderr", 256 * 1024],
  ]) {
    const directory = await mkdtemp(join(tmpdir(), `ctx-ts-${mode}-`));
    const completedPath = join(directory, "completed");
    try {
      await assert.rejects(
        () => adapter.execute([subprocessFixture, mode, completedPath]),
        (error) => {
          assert.ok(error instanceof CtxParseError);
          assert.equal(error.message, "ctx CLI output exceeded its capture limit");
          assert.equal(error.code, "capture_limit");
          assert.deepEqual(error.details, {
            command: process.execPath,
            args: [subprocessFixture, mode, completedPath],
            stream,
            capBytes,
          });
          assert.ok(!Object.hasOwn(error.details, "stdout"));
          assert.ok(!Object.hasOwn(error.details, "stderr"));
          return true;
        },
      );
      assert.equal(await readFile(completedPath, "utf8"), "completed");
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  }
});

test("local subprocess preserves nonzero and malformed JSON error behavior", async () => {
  const adapter = new LocalCliAdapter({ ctxPath: process.execPath, timeoutMs: 2_000 });
  const subprocessClient = (mode) =>
    createLocalAgentHistoryClient({
      runner: () => adapter.execute([subprocessFixture, mode]),
    });

  await assert.rejects(
    () => subprocessClient("nonzero").status(),
    (error) => {
      assert.ok(error instanceof CtxCliError);
      assert.equal(error.message, "ctx status --format=json failed");
      assert.equal(error.exitCode, 2);
      assert.equal(error.stdout, '{"partial":true}\n');
      assert.equal(error.stderr, "bad flag\n");
      assert.deepEqual(error.details, {
        command: process.execPath,
        args: [subprocessFixture, "nonzero"],
        exitCode: 2,
        signal: null,
        stdout: '{"partial":true}\n',
        stderr: "bad flag\n",
      });
      return true;
    },
  );

  await assert.rejects(
    () => subprocessClient("malformed").status(),
    (error) => {
      assert.ok(error instanceof CtxParseError);
      assert.equal(error.message, "ctx returned invalid JSON: trailing data");
      assert.deepEqual(error.details, { offset: 4 });
      return true;
    },
  );
});

async function assertProcessExited(pid) {
  const deadline = performance.now() + 2_000;
  while (performance.now() < deadline) {
    try {
      process.kill(pid, 0);
    } catch (error) {
      if (error.code === "ESRCH") return;
      throw error;
    }
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.fail(`owned process ${pid} survived bounded teardown`);
}

async function forceProcessExit(pid) {
  try {
    process.kill(pid, "SIGKILL");
  } catch (error) {
    if (error.code === "ESRCH") return;
    throw error;
  }
  await assertProcessExited(pid);
}

test("hosted client is an explicit placeholder", async () => {
  const client = createHostedAgentHistoryClient({ baseUrl: "https://ctx.example.invalid" });

  assert.equal((await client.version()).adapter, "hosted-placeholder");
  await assert.rejects(() => client.status(), CtxUnsupportedError);
});

test("dogfood toy app runs status/search/show with mocked ctx", async () => {
  assert.deepEqual(await runDogfoodToy({ env: {} }), {
    ready: true,
    query: "local agent history",
    firstScope: "event",
    returned: 1,
    moreAvailable: true,
    eventCount: 1,
    sessionMode: "lite",
    eventProviderSession: "codex-fixture-session",
    sessionProviderSession: "codex-fixture-session",
  });
});

test("shared agent-history-v1 fixtures use discriminated operation payloads", async () => {
  const fixturesDir = join(repoRoot, "contracts", "agent-history-v1", "fixtures");
  let entries = [];
  try {
    entries = await readdir(fixturesDir);
  } catch (error) {
    if (error.code !== "ENOENT") {
      throw error;
    }
  }

  const fixtureFiles = entries.filter((name) => name.endsWith(".json"));
  assert.notEqual(fixtureFiles.length, 0, "agent-history-v1 fixture directory should not be empty");
  for (const entry of fixtureFiles) {
    const fixture = JSON.parse(await readFile(join(fixturesDir, entry), "utf8"));
    const operation = operationFromFixtureName(entry);
    assert.equal(typeof fixture, "object", `${entry} should contain a JSON object`);
    assert.equal(fixture.contractVersion, AGENT_HISTORY_V1_VERSION, `${entry} contractVersion`);
    assert.equal(fixture.schemaVersion, 1, `${entry} schemaVersion`);
    assert.equal(fixture.operation, operation, `${entry} operation`);
    assertFixturePayload(entry, fixture);
  }
});

function operationFromFixtureName(name) {
  const operation = name.split(".")[0];
  switch (operation) {
    case "status":
    case "init":
    case "sources":
    case "import":
    case "sync":
    case "search":
    case "error":
      return operation;
    case "show-event":
      return "showEvent";
    case "show-session":
      return "showSession";
    default:
      throw new Error(`unknown agent-history-v1 fixture operation in ${name}`);
  }
}

function assertFixturePayload(entry, fixture) {
  switch (fixture.operation) {
    case "status":
    case "init":
      assert.equal(typeof fixture.status.initialized, "boolean", `${entry} status.initialized`);
      assert.equal(typeof fixture.status.localOnly, "boolean", `${entry} status.localOnly`);
      break;
    case "sources":
      assert.ok(Array.isArray(fixture.sources), `${entry} sources`);
      assert.equal(typeof fixture.sources[0].provider, "string", `${entry} sources[0].provider`);
      assert.equal(typeof fixture.sources[0].importable, "boolean", `${entry} sources[0].importable`);
      break;
    case "import":
    case "sync":
      assert.equal(typeof fixture.import.resume, "boolean", `${entry} import.resume`);
      assert.equal(typeof fixture.import.totals, "object", `${entry} import.totals`);
      break;
    case "search":
      assert.ok(Array.isArray(fixture.search.results), `${entry} search.results`);
      assert.equal(
        fixture.search.resultWindow.returned,
        fixture.search.results.length,
        `${entry} search.resultWindow.returned`,
      );
      assert.equal(
        fixture.search.pagination.hasMore,
        fixture.search.resultWindow.moreAvailable,
        `${entry} search.pagination.hasMore`,
      );
      if (fixture.search.results.length > 0) {
        assert.equal(
          typeof fixture.search.results[0].resultScope,
          "string",
          `${entry} search.results[0].resultScope`,
        );
        assert.equal(fixture.search.results[0].rank, 1, `${entry} search.results[0].rank`);
        assert.equal(
          fixture.search.results[0].sourceFormat,
          "codex_session_jsonl",
          `${entry} search.results[0].sourceFormat`,
        );
        assert.equal(
          fixture.search.results[0].retrievalScore,
          0.98,
          `${entry} search.results[0].retrievalScore`,
        );
      }
      break;
    case "showEvent":
      assert.ok(Array.isArray(fixture.event.events), `${entry} event.events`);
      assert.equal(typeof fixture.event.events[0].ctxEventId, "string", `${entry} event id`);
      assert.equal(fixture.event.events[0].content.complete, true, `${entry} content complete`);
      assert.equal(fixture.event.events[0].content.policyStatus, "selected", `${entry} policy`);
      break;
    case "showSession":
      assert.ok(Array.isArray(fixture.session.events), `${entry} session.events`);
      assert.equal(typeof fixture.session.mode, "string", `${entry} session.mode`);
      assert.equal(
        fixture.session.session.providerSessionId,
        "codex-fixture-session",
        `${entry} resume id`,
      );
      assert.equal(
        fixture.session.session.sourceFormat,
        "codex_session_jsonl",
        `${entry} session source format`,
      );
      break;
    case "error":
      assert.equal(typeof fixture.error.code, "string", `${entry} error.code`);
      assert.equal(typeof fixture.error.retryable, "boolean", `${entry} error.retryable`);
      break;
    default:
      throw new Error(`unsupported fixture operation ${fixture.operation} in ${entry}`);
  }
}
