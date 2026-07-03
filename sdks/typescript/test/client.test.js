import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";
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
  createHostedAgentHistoryClient,
  createLocalAgentHistoryClient,
} from "../src/index.js";
import { runDogfoodToy } from "../examples/dogfood-toy.js";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

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
  await client.init({ catalogOnly: true });
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
      ["--data-root", "/tmp/ctx-sdk-test", "status", "--json"],
      [
        "--data-root",
        "/tmp/ctx-sdk-test",
        "setup",
        "--json",
        "--progress",
        "none",
        "--catalog-only",
      ],
      ["--data-root", "/tmp/ctx-sdk-test", "sources", "--json"],
      [
        "--data-root",
        "/tmp/ctx-sdk-test",
        "import",
        "--json",
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
        "--json",
        "--progress",
        "none",
        "--all",
      ],
    ],
  );
});

test("builds search flags and normalizes nested CLI search output", async () => {
  const { client, calls } = mockClient(() =>
    JSON.stringify({
      query: "retry handling",
      generated_at: "2026-07-01T12:00:00Z",
      freshness: { mode: "off", status: "skipped", source_count: 1, totals: {} },
      results: [
        {
          ctx_event_id: "00000000-0000-0000-0000-000000000101",
          ctx_session_id: "00000000-0000-0000-0000-000000000102",
          provider_session_id: "codex-session",
          event_seq: 7,
          result_scope: "event",
          source_path: "/tmp/session.jsonl",
          source_exists: true,
          why_matched: ["text"],
          citations: [
            {
              ctx_event_id: "00000000-0000-0000-0000-000000000101",
              ctx_session_id: "00000000-0000-0000-0000-000000000102",
              source_path: "/tmp/session.jsonl",
              source_exists: true,
            },
          ],
        },
      ],
      pagination: { next_cursor: "page-2", has_more: true },
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
  assert.equal(result.search.results[0].eventSeq, 7);
  assert.equal(result.search.results[0].resultScope, "event");
  assert.equal(result.search.results[0].sourcePath, "/tmp/session.jsonl");
  assert.equal(result.search.results[0].sourceExists, true);
  assert.equal(result.search.results[0].whyMatched[0], "text");
  assert.equal(result.search.results[0].citations[0].sourcePath, "/tmp/session.jsonl");
  assert.equal(result.search.pagination.nextCursor, "page-2");
  assert.equal(result.search.pagination.hasMore, true);

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
    "--refresh",
    "off",
    "--include-current-session",
    "--json",
  ]);
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

test("wraps show and locate commands by ctx id and provider session id", async () => {
  const { client, calls } = mockClient(() => "{}");

  await client.showEvent("00000000-0000-0000-0000-000000000002", { window: 3 });
  await client.showSession("00000000-0000-0000-0000-000000000003", { mode: "full" });
  await client.showSession({ provider: "codex", providerSession: "codex-session", mode: "log" });
  await client.locateEvent("00000000-0000-0000-0000-000000000004");
  await client.locateSession({ provider: "codex", providerSession: "codex-session" });

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
      ["locate", "event", "00000000-0000-0000-0000-000000000004", "--format", "json"],
      [
        "locate",
        "session",
        "--provider",
        "codex",
        "--provider-session",
        "codex-session",
        "--format",
        "json",
      ],
    ],
  );
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
  await assert.rejects(() => cli.status(), CtxCliError);

  const parse = createLocalAgentHistoryClient({ runner: () => "not json" });
  await assert.rejects(() => parse.status(), CtxParseError);

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

test("hosted client is an explicit placeholder", async () => {
  const client = createHostedAgentHistoryClient({ baseUrl: "https://ctx.example.invalid" });

  assert.equal((await client.version()).adapter, "hosted-placeholder");
  await assert.rejects(() => client.status(), CtxUnsupportedError);
});

test("dogfood toy app runs status/search/show/locate with mocked ctx", async () => {
  assert.deepEqual(await runDogfoodToy({ env: {} }), {
    ready: true,
    query: "local agent history",
    firstScope: "event",
    eventCount: 1,
    sessionMode: "lite",
    eventPath: "/tmp/ctx-sdk-dogfood/session.jsonl",
    sessionPath: "/tmp/ctx-sdk-dogfood/session.jsonl",
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
    case "locate-event":
      return "locateEvent";
    case "locate-session":
      return "locateSession";
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
      if (fixture.search.results.length > 0) {
        assert.equal(
          typeof fixture.search.results[0].resultScope,
          "string",
          `${entry} search.results[0].resultScope`,
        );
      }
      break;
    case "showEvent":
      assert.ok(Array.isArray(fixture.event.events), `${entry} event.events`);
      assert.equal(typeof fixture.event.events[0].ctxEventId, "string", `${entry} event id`);
      break;
    case "showSession":
      assert.ok(Array.isArray(fixture.session.events), `${entry} session.events`);
      assert.equal(typeof fixture.session.mode, "string", `${entry} session.mode`);
      break;
    case "locateEvent":
    case "locateSession":
      assert.equal(typeof fixture.location.ctxSessionId, "string", `${entry} location session id`);
      assert.equal(typeof fixture.location.provider, "string", `${entry} location provider`);
      assert.equal(typeof fixture.location.source, "object", `${entry} location source`);
      break;
    case "error":
      assert.equal(typeof fixture.error.code, "string", `${entry} error.code`);
      assert.equal(typeof fixture.error.retryable, "boolean", `${entry} error.retryable`);
      break;
    default:
      throw new Error(`unsupported fixture operation ${fixture.operation} in ${entry}`);
  }
}
