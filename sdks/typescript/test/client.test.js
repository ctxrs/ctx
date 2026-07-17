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
  createHostedAgentHistoryClient,
  createLocalAgentHistoryClient,
  serializeSearchQuery,
} from "../src/index.js";
import { runDogfoodToy } from "../examples/dogfood-toy.js";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const SEARCH_QUERY = {
  version: "ctx-search-v1",
  any: [
    { all: "disk io pressure" },
    { phrase: "storage latency" },
    { literal: "logs_2.db" },
    { semantic: "the indexing job made the workstation sluggish" },
  ],
  must: [{ all: "codex" }],
  must_not: [{ phrase: "postgres vacuum" }],
};

function searchExecution() {
  return {
    query_version: "ctx-search-v1",
    candidate_strategy: "bounded_rrf_v1",
    resolved: {
      query_bytes: 8192,
      clauses: 32,
      analyzed_tokens_per_clause: 32,
      candidates_per_positive_seed: 1024,
      candidate_rows: 16384,
      retained_candidate_ids: 8192,
      residual_rows: 8192,
      verification_bytes: 16777216,
      verification_lookup_bytes: 16384,
      hydrated_rows: 256,
      hydration_input_bytes: 8388608,
      hydration_input_bytes_per_event: 65536,
      snippet_input_bytes: 8388608,
      returned_text_bytes: 524288,
      serialized_response_bytes: 2097152,
      results: 5,
      elapsed_ms: 2500,
    },
    consumed: {
      query_bytes: 96,
      clauses: 7,
      analyzed_tokens: 18,
      largest_analyzed_tokens_per_clause: 6,
      largest_positive_seed_candidates: 20,
      candidate_rows: 48,
      retained_candidate_ids: 31,
      residual_rows: 12,
      verification_bytes: 4096,
      largest_verification_lookup_bytes: 512,
      hydrated_rows: 5,
      hydration_input_bytes: 2048,
      largest_hydration_input_bytes: 800,
      snippet_input_bytes: 1200,
      returned_results: 1,
      returned_text_bytes: 128,
      serialized_response_bytes: 2048,
      elapsed_ms: 12,
    },
    semantic: {
      attempted: true,
      required: true,
      readiness: "ready",
      effective_backend: "hybrid",
      requested_candidates: 20,
      eligible_candidates: 18,
      candidates_supplied: 20,
      candidates_consumed: 18,
      candidates_used: 4,
      coverage: { indexed_documents: 990, searchable_documents: 1000 },
      completeness: "partial",
      incompleteness_reasons: ["semantic_coverage_incomplete"],
      positive_text_rule_version: "ctx-search-positive-text-v1",
    },
    requested_result_limit: 5,
    result_limit: 5,
    max_result_limit: 200,
    rrf_k: 60,
    per_branch_candidate_rows: 1024,
    clauses_executed: 7,
    verification_dropped: 0,
    filter_verification_dropped: 0,
    candidate_budget_exhausted: false,
    timed_out: false,
    truncated: true,
    truncation_reasons: ["semantic_coverage_incomplete"],
  };
}

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
      schema_version: 2,
      query: SEARCH_QUERY,
      query_execution: searchExecution(),
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
          event_seq: 7,
          result_type: "event",
          result_scope: "event",
          source_path: "/tmp/session.jsonl",
          source_exists: true,
          why_matched: ["text"],
          citations: [
            {
              target_type: "event",
              ctx_event_id: "00000000-0000-0000-0000-000000000101",
              ctx_session_id: "00000000-0000-0000-0000-000000000102",
              source_path: "/tmp/session.jsonl",
              source_exists: true,
            },
          ],
        },
      ],
      pagination: { next_cursor: "page-2", has_more: true },
      truncation: {
        truncated: true,
        reason: "semantic_coverage_incomplete",
        omitted_results: 1,
      },
    }),
  );

  const result = await client.search(SEARCH_QUERY, {
    limit: 5,
    provider: "custom",
    historySource: "dorkos/default",
    providerKey: "dorkos",
    sourceId: "default",
    sourceFormat: "dorkos-history-v1",
    workspace: "ctx",
    since: "30d",
    primaryOnly: true,
    eventType: "message",
    file: "crates/foo/src/lib.rs",
    session: "00000000-0000-0000-0000-000000000001",
    events: true,
    backend: "hybrid",
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
  assert.equal(result.search.results[0].resultType, "event");
  assert.equal(result.search.results[0].resultScope, "event");
  assert.equal(result.search.results[0].sourcePath, "/tmp/session.jsonl");
  assert.equal(result.search.results[0].sourceExists, true);
  assert.equal(result.search.results[0].whyMatched[0], "text");
  assert.equal(result.search.results[0].citations[0].targetType, "event");
  assert.equal(result.search.results[0].citations[0].sourcePath, "/tmp/session.jsonl");
  assert.equal(result.search.retrieval.requestedMode, "hybrid");
  assert.equal(result.search.retrieval.effectiveMode, "lexical");
  assert.equal("semanticWeight" in result.search.retrieval, false);
  assert.equal("semanticFallbackCode" in result.search.retrieval, false);
  assert.equal("semanticFallback" in result.search.retrieval, false);
  assert.equal(result.search.retrieval.coverage.embeddedItems, 4);
  assert.equal(result.search.retrieval.coverage.indexedNow, 1);
  assert.equal(result.search.retrieval.diagnostics.queryEmbedMs, 2);
  assert.equal(result.search.pagination.nextCursor, "page-2");
  assert.equal(result.search.pagination.hasMore, true);
  assert.equal(result.search.schema_version, 2);
  assert.deepEqual(result.search.query.must_not, SEARCH_QUERY.must_not);
  assert.equal(result.search.query_execution.resolved.verification_bytes, 16777216);
  assert.equal(result.search.query_execution.consumed.snippet_input_bytes, 1200);
  assert.equal(result.search.query_execution.requested_result_limit, 5);
  assert.equal(result.search.query_execution.consumed.candidate_rows, 48);
  assert.equal(result.search.query_execution.semantic.readiness, "ready");
  assert.equal(result.search.query_execution.semantic.coverage.indexed_documents, 990);
  assert.equal(result.search.query_execution.semantic.completeness, "partial");
  assert.equal("queryExecution" in result.search, false);
  assert.equal("verificationBytes" in result.search.query_execution.resolved, false);

  assert.deepEqual(calls[0].args, [
    "--data-root",
    "/tmp/ctx-sdk-test",
    "search",
    "--query-json",
    serializeSearchQuery(SEARCH_QUERY),
    "--limit",
    "5",
    "--provider",
    "custom",
    "--history-source",
    "dorkos/default",
    "--provider-key",
    "dorkos",
    "--source-id",
    "default",
    "--source-format",
    "dorkos-history-v1",
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
    "--refresh",
    "off",
    "--include-current-session",
    "--json",
  ]);
});

test("omits backend override when unset", async () => {
  const { client, calls } = mockClient(() =>
    JSON.stringify({
      schema_version: 2,
      query: SEARCH_QUERY,
      query_execution: searchExecution(),
      results: [],
    }),
  );

  await client.search(SEARCH_QUERY);

  assert.equal(calls[0].args.includes("--backend"), false);
});

test("rejects search without a structured query or file before invoking CLI", async () => {
  const { client, calls } = mockClient(() => {
    throw new Error("runner should not be called");
  });

  await assert.rejects(() => client.search(), CtxValidationError);
  await assert.rejects(() => client.search({ refresh: "off", limit: 5 }), CtxValidationError);
  await assert.rejects(() => client.search("   "), CtxValidationError);

  assert.equal(calls.length, 0);
});

test("validates every ctx-search-v1 matcher and rejects ambiguous shapes", () => {
  assert.deepEqual(JSON.parse(serializeSearchQuery(SEARCH_QUERY)), SEARCH_QUERY);
  const invalid = [
    { version: "ctx-search-v1", must_not: [{ all: "only negative" }] },
    { version: "ctx-search-v1", any: [{ semantic: "one" }, { semantic: "two" }] },
    { version: "ctx-search-v1", must: [{ semantic: "wrong placement" }] },
    { version: "ctx-search-v1", any: [{ all: "x", phrase: "x" }] },
    { version: "ctx-search-v1", any: [{ literal: "x" }] },
    { version: "ctx-search-v1", any: [{ all: "x" }], unknown: true },
    { version: "ctx-search-v1", any: [{ all: "x" }], mustNot: [] },
    {
      version: "ctx-search-v1",
      any: Array.from({ length: 33 }, (_, index) => ({ all: `term-${index}` })),
    },
    { version: "ctx-search-v1", any: [{ all: "x".repeat(1025) }] },
    { version: "ctx-search-v1", any: [{ all: "!!!" }] },
    { version: "ctx-search-v1", any: [{ all: Array.from({ length: 33 }, () => "x").join(" ") }] },
  ];
  for (const query of invalid) {
    assert.throws(() => serializeSearchQuery(query), CtxValidationError);
  }
});

test("canonicalizes and deduplicates ctx-search-v1 clauses before enforcing bounds", () => {
  const canonical = JSON.parse(
    serializeSearchQuery({
      version: "ctx-search-v1",
      any: [
        { all: "  disk\t io  pressure " },
        { all: "disk io pressure" },
        { literal: "  logs_2.db  raw  " },
      ],
      must: [],
      must_not: [{ phrase: " postgres\n vacuum " }],
    }),
  );
  assert.deepEqual(canonical, {
    version: "ctx-search-v1",
    any: [{ all: "disk io pressure" }, { literal: "logs_2.db  raw" }],
    must_not: [{ phrase: "postgres vacuum" }],
  });

  const deduped = JSON.parse(
    serializeSearchQuery({
      version: "ctx-search-v1",
      any: Array.from({ length: 33 }, () => ({ all: "  cafe\u0301\u00a0\u4e16\u754c  " })),
    }),
  );
  assert.deepEqual(deduped.any, [{ all: "cafe\u0301 \u4e16\u754c" }]);
});

test("validates search limits before invoking local or hosted transports", async () => {
  const { client, calls } = mockClient(() => {
    throw new Error("runner should not be called");
  });
  const hosted = createHostedAgentHistoryClient();

  for (const limit of [0, 201, 1.5, Number.NaN]) {
    await assert.rejects(() => client.search(SEARCH_QUERY, { limit }), CtxValidationError);
    await assert.rejects(() => hosted.search(SEARCH_QUERY, { limit }), CtxValidationError);
  }
  assert.equal(calls.length, 0);

  const accepted = mockClient(() =>
    JSON.stringify({
      schema_version: 2,
      query: SEARCH_QUERY,
      query_execution: searchExecution(),
      results: [],
    }),
  );
  await accepted.client.search(SEARCH_QUERY, { limit: 1 });
  await accepted.client.search(SEARCH_QUERY, { limit: 200 });
  assert.equal(accepted.calls[0].args.includes("1"), true);
  assert.equal(accepted.calls[1].args.includes("200"), true);
});

test("rejects the pre-v2 ambiguous search response", async () => {
  const { client } = mockClient(() =>
    JSON.stringify({ schema_version: 1, query: "old ambiguous query", results: [] }),
  );

  await assert.rejects(() => client.search(SEARCH_QUERY), CtxParseError);

  const aliasOnly = mockClient(() =>
    JSON.stringify({
      schema_version: 2,
      query: SEARCH_QUERY,
      queryExecution: searchExecution(),
      results: [],
    }),
  );
  await assert.rejects(() => aliasOnly.client.search(SEARCH_QUERY), CtxParseError);

  for (const [field, payload] of [
    ["query", { schema_version: 2, query_execution: {}, results: [] }],
    ["query_execution", { schema_version: 2, query: null, results: [] }],
    ["results", { schema_version: 2, query: null, query_execution: {} }],
    ["results", { schema_version: 2, query: null, query_execution: {}, results: {} }],
  ]) {
    const missing = mockClient(() => JSON.stringify(payload));
    await assert.rejects(
      () => missing.client.search(SEARCH_QUERY),
      (error) => error instanceof CtxParseError && error.details.field === field,
    );
  }
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

test("Windows local adapter fails closed without a process-scope launcher", async (context) => {
  if (process.platform !== "win32") {
    context.skip("Windows-only containment contract");
    return;
  }
  const { LocalCliAdapter } = await import("../src/index.js");
  const adapter = new LocalCliAdapter({
    ctxPath: process.execPath,
    env: { CTX_SDK_PROCESS_SCOPE_LAUNCHER: "" },
  });
  await assert.rejects(
    () => adapter.execute(["--version"]),
    (error) =>
      error instanceof CtxParseError &&
      error.code === "backend_unavailable" &&
      error.details.backend === "process_scope",
  );
});

test("local adapter drains both streams and enforces byte caps", async () => {
  const { LocalCliAdapter } = await import("../src/index.js");
  const adapter = new LocalCliAdapter({ ctxPath: process.execPath, timeoutMs: 2_000 });
  const completed = await adapter.execute([
    "-e",
    "process.stdout.write(Buffer.alloc(200000,97));process.stderr.write(Buffer.alloc(200000,98))",
  ]);
  assert.equal(Buffer.byteLength(completed.stdout), 200000);
  assert.equal(Buffer.byteLength(completed.stderr), 200000);

  const alternating = await adapter.execute([
    "-e",
    "const {spawn}=require('node:child_process');let n=0;const done=()=>{if(++n===2)process.exit(0)};spawn(process.execPath,['-e','process.stdout.write(Buffer.alloc(245760,97))'],{stdio:['ignore',1,'ignore']}).on('exit',done);spawn(process.execPath,['-e','process.stdout.write(Buffer.alloc(245760,98))'],{stdio:['ignore',2,'ignore']}).on('exit',done)",
  ]);
  assert.equal(Buffer.byteLength(alternating.stdout), 30 * 8192);
  assert.equal(Buffer.byteLength(alternating.stderr), 30 * 8192);

  for (const [stream, descriptor, size, capBytes] of [
    ["stdout", "stdout", 2 * 1024 * 1024 + 1, 2 * 1024 * 1024],
    ["stderr", "stderr", 256 * 1024 + 1, 256 * 1024],
  ]) {
    await assert.rejects(
      () =>
        adapter.execute([
          "-e",
          `process.${descriptor}.write(Buffer.alloc(${size},120))`,
        ]),
      (error) =>
        error instanceof CtxParseError &&
        error.code === "capture_limit" &&
        error.details.stream === stream &&
        error.details.capBytes === capBytes &&
        !("stdout" in error.details) &&
        !("stderr" in error.details),
    );
  }

  const started = Date.now();
  await assert.rejects(
    () =>
      adapter.execute([
        "-e",
        "process.stderr.write(Buffer.alloc(262145,120));setInterval(()=>process.stdout.write('x'),10)",
      ]),
    (error) =>
      error instanceof CtxParseError &&
      error.code === "capture_limit" &&
      error.details.stream === "stderr",
  );
  assert.ok(Date.now() - started < 2_000);
});

test("local adapter bounds inherited-pipe teardown", async () => {
  if (process.platform === "win32" && !process.env.CTX_SDK_PROCESS_SCOPE_LAUNCHER) return;
  const { LocalCliAdapter } = await import("../src/index.js");
  const adapter = new LocalCliAdapter({ ctxPath: process.execPath, timeoutMs: 5_000 });
  const directory = await mkdtemp(join(tmpdir(), "ctx-ts-scope-"));
  const pidPath = join(directory, "child.pid");
  try {
    const started = Date.now();
    await assert.rejects(
      () =>
        adapter.execute([
          "-e",
          `const fs=require('node:fs');const c=require('node:child_process').spawn(process.execPath,['-e','process.on("SIGTERM",()=>{});setTimeout(()=>{},60000)'],{stdio:'inherit'});fs.writeFileSync(${JSON.stringify(pidPath)},String(c.pid));`,
        ]),
      (error) => error instanceof CtxParseError && error.code === "capture_failure",
    );
    assert.ok(Date.now() - started < 2_000);
    await assertProcessExited(Number(await readFile(pidPath, "utf8")));
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("successful local command kills same-scope child with closed pipes", async () => {
  if (process.platform === "win32" && !process.env.CTX_SDK_PROCESS_SCOPE_LAUNCHER) return;
  const { LocalCliAdapter } = await import("../src/index.js");
  const adapter = new LocalCliAdapter({ ctxPath: process.execPath, timeoutMs: 2_000 });
  const directory = await mkdtemp(join(tmpdir(), "ctx-ts-success-scope-"));
  const pidPath = join(directory, "child.pid");
  let pid;
  try {
    const completed = await adapter.execute([
      "-e",
      `const fs=require('node:fs');const c=require('node:child_process').spawn(process.execPath,['-e','process.on("SIGTERM",()=>{});setTimeout(()=>{},60000)'],{stdio:'ignore'});fs.writeFileSync(${JSON.stringify(pidPath)},String(c.pid));process.stdout.write('{}');`,
    ]);
    assert.equal(completed.stdout, "{}");
    pid = Number(await readFile(pidPath, "utf8"));
    await assertProcessExited(pid);
    pid = undefined;
  } finally {
    if (pid) {
      try {
        process.kill(pid, "SIGKILL");
      } catch {}
      await assertProcessExited(pid);
    }
    await rm(directory, { recursive: true, force: true });
  }
});

async function assertProcessExited(pid) {
  const deadline = Date.now() + 1_000;
  while (Date.now() < deadline) {
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

test("hosted client is an explicit placeholder", async () => {
  const client = createHostedAgentHistoryClient({ baseUrl: "https://ctx.example.invalid" });

  assert.equal((await client.version()).adapter, "hosted-placeholder");
  await assert.rejects(() => client.status(), CtxUnsupportedError);
  await assert.rejects(() => client.search(SEARCH_QUERY), CtxUnsupportedError);
  await assert.rejects(
    () => client.search({ version: "ctx-search-v1", must_not: [{ all: "negative" }] }),
    CtxValidationError,
  );
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
