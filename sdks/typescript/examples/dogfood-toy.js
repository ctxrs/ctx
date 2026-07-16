import { createLocalAgentHistoryClient } from "../src/index.js";

const EVENT_ID = "11111111-1111-4111-8111-111111111111";
const SESSION_ID = "22222222-2222-4222-8222-222222222222";
const PROVIDER_SESSION_ID = "codex-fixture-session";
const SOURCE_PATH = "/tmp/ctx-sdk-dogfood/session.jsonl";

export function createDogfoodClient(options = {}) {
  const env = options.env ?? process.env;
  const ctxPath = env.CTX_SDK_EXAMPLE_CTX_PATH;
  if (ctxPath) {
    return createLocalAgentHistoryClient({
      ctxPath,
      dataRoot: env.CTX_SDK_EXAMPLE_DATA_ROOT,
      cwd: env.CTX_SDK_EXAMPLE_CWD,
      timeoutMs: Number(env.CTX_SDK_EXAMPLE_TIMEOUT_MS ?? 60_000),
    });
  }

  return createLocalAgentHistoryClient({
    dataRoot: "/tmp/ctx-sdk-dogfood",
    runner: dogfoodMockRunner,
  });
}

export async function runDogfoodToy(options = {}) {
  const client = options.client ?? createDogfoodClient(options);
  const status = await client.status();
  const search = await client.search({
    version: "ctx-search-v1",
    any: [{ all: "local agent history" }],
  }, {
    limit: 3,
    provider: "codex",
    refresh: "off",
  });
  const firstHit = search.search.results[0];
  const eventId = firstHit?.ctxEventId ?? EVENT_ID;
  const sessionId = firstHit?.ctxSessionId ?? SESSION_ID;

  const event = await client.showEvent(eventId, { window: 1 });
  const session = await client.showSession(sessionId, { mode: "lite" });
  const eventLocation = await client.locateEvent(eventId);
  const sessionLocation = await client.locateSession(sessionId);

  return {
    ready: status.status.initialized,
    query: search.search.query.any?.[0]?.all ?? null,
    firstScope: firstHit?.resultScope ?? null,
    eventCount: event.event.events.length,
    sessionMode: session.session.mode,
    eventPath: eventLocation.location.source.path,
    sessionPath: sessionLocation.location.source.path,
  };
}

export async function dogfoodMockRunner({ args }) {
  const command = argsWithoutDataRoot(args);
  const stdout = JSON.stringify(mockResponse(command));
  return { exitCode: 0, stdout, stderr: "" };
}

function argsWithoutDataRoot(args) {
  if (args[0] === "--data-root") {
    return args.slice(2);
  }
  return args;
}

function mockResponse(args) {
  const [command, subcommand] = args;
  if (command === "status") {
    return {
      initialized: true,
      local_only: true,
      data_root: "/tmp/ctx-sdk-dogfood",
      indexed_items: 1,
      indexed_sources: 1,
    };
  }
  if (command === "search") {
    const query = JSON.parse(args[args.indexOf("--query-json") + 1]);
    return {
      schema_version: 2,
      query,
      query_execution: {
        query_version: "ctx-search-v1",
        resolved: {},
        consumed: {},
        semantic: {},
        truncated: false,
        truncation_reasons: [],
      },
      generated_at: "2026-07-01T12:00:00Z",
      freshness: { mode: "off", status: "skipped", source_count: 0, totals: {} },
      results: [
        {
          ctx_event_id: EVENT_ID,
          ctx_session_id: SESSION_ID,
          provider_session_id: PROVIDER_SESSION_ID,
          event_seq: 1,
          result_type: "event",
          result_scope: "event",
          provider: "codex",
          snippet: "local agent history search result",
          source_path: SOURCE_PATH,
          source_exists: true,
          cursor: "line:2",
        },
      ],
    };
  }
  if (command === "show" && subcommand === "event") {
    return {
      event: mockEvent(),
      events: [mockEvent()],
      source: mockSource(),
    };
  }
  if (command === "show" && subcommand === "session") {
    return {
      session: {
        ctx_session_id: SESSION_ID,
        provider: "codex",
        provider_session_id: PROVIDER_SESSION_ID,
      },
      events: [mockEvent()],
      source: mockSource(),
      mode: "lite",
      format: "json",
    };
  }
  if (command === "locate" && (subcommand === "event" || subcommand === "session")) {
    return {
      ctx_session_id: SESSION_ID,
      ctx_event_id: subcommand === "event" ? EVENT_ID : null,
      provider: "codex",
      provider_session_id: PROVIDER_SESSION_ID,
      source: mockSource(),
      resume: { cursor: "line:2" },
    };
  }
  throw new Error(`unexpected mocked ctx command: ${args.join(" ")}`);
}

function mockEvent() {
  return {
    ctx_event_id: EVENT_ID,
    ctx_session_id: SESSION_ID,
    sequence: 1,
    event_type: "message",
    role: "assistant",
    occurred_at: "2026-07-01T12:00:00Z",
    source: "codex",
    cursor: "line:2",
    text: "local agent history search result",
  };
}

function mockSource() {
  return {
    path: SOURCE_PATH,
    cursor: "line:2",
    exists: true,
    source_id: "33333333-3333-4333-8333-333333333333",
    source_format: "codex_session_jsonl",
  };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const result = await runDogfoodToy();
  console.log(JSON.stringify(result, null, 2));
}
