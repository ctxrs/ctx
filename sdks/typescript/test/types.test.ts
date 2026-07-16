import {
  type ImportEnvelope,
  type JsonValue,
  type LocationEnvelope,
  type AgentHistoryEnvelope,
  type SearchBackendMode,
  type SearchEnvelope,
  type SearchQueryV1,
  type ShowEventEnvelope,
  type SourcesEnvelope,
  type StatusEnvelope,
  createLocalAgentHistoryClient,
  toAgentHistoryEnvelope,
} from "../src/index.js";

function expectType<T>(_value: T): void {}

const client = createLocalAgentHistoryClient({
  runner: () => "{}",
});

const query: SearchQueryV1 = {
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

const status = await client.status();
expectType<StatusEnvelope>(status);
expectType<"status">(status.operation);
expectType<boolean>(status.status.initialized);
// @ts-expect-error status envelopes do not expose a search payload.
status.search.results;

const sources = await client.sources();
expectType<SourcesEnvelope>(sources);
expectType<string>(sources.sources[0]!.provider);
expectType<boolean>(sources.sources[0]!.importable);

const imported = await client.import({ provider: "codex" });
expectType<ImportEnvelope<"import">>(imported);
expectType<"import">(imported.operation);
expectType<number | undefined>(imported.import.totals.importedEvents);

const synced = await client.sync({ all: true });
expectType<ImportEnvelope<"sync">>(synced);
expectType<"sync">(synced.operation);

const search = await client.search(query, { refresh: "off" });
expectType<SearchEnvelope>(search);
expectType<string | null | undefined>(search.search.results[0]!.resultType);
expectType<string>(search.search.results[0]!.resultScope);
expectType<string | null | undefined>(search.search.results[0]!.ctxEventId);
expectType<string | null | undefined>(search.search.results[0]!.citations?.[0]?.targetType);
expectType<SearchBackendMode | string | null | undefined>(search.search.retrieval?.requestedMode);
expectType<number | null | undefined>(search.search.retrieval?.semanticWeight);
expectType<string | null | undefined>(search.search.retrieval?.semanticFallbackCode);
expectType<number | undefined>(search.search.retrieval?.coverage?.embeddedItems);
expectType<JsonValue | undefined>(search.search.retrieval?.diagnostics?.queryEmbedMs);
expectType<2>(search.search.schema_version);
expectType<SearchQueryV1 | null>(search.search.query);
expectType<number>(search.search.query_execution.resolved.verification_bytes);
expectType<number>(search.search.query_execution.consumed.snippet_input_bytes);
expectType<number>(search.search.query_execution.requested_result_limit);
expectType<number>(search.search.query_execution.result_limit);
expectType<"ready" | "not_ready" | "unsupported" | "unavailable">(
  search.search.query_execution.semantic.readiness,
);
expectType<number | undefined>(search.search.query_execution.semantic.coverage?.indexed_documents);
expectType<"complete" | "partial" | undefined>(search.search.query_execution.semantic.completeness);
// @ts-expect-error search results expose ctxEventId, not ctx_event_id.
search.search.results[0]!.ctx_event_id;

const semanticSearch = await client.search(query, { backend: "hybrid" });
expectType<SearchEnvelope>(semanticSearch);

const objectSearch = await client.search({ query, refresh: "off" });
expectType<SearchEnvelope>(objectSearch);
const fileSearch = await client.search({ file: "src/lib.rs", refresh: "off" });
expectType<SearchEnvelope>(fileSearch);
// @ts-expect-error search requires a structured query or file option.
await client.search();
// @ts-expect-error search filters alone are not a search intent.
await client.search({ refresh: "off", limit: 5 });
// @ts-expect-error backend alone is not a search intent.
await client.search({ backend: "hybrid" });
const invalidSemanticPlacement: SearchQueryV1 = {
  version: "ctx-search-v1",
  // @ts-expect-error semantic clauses are not allowed in must.
  must: [{ semantic: "wrong placement" }],
};

const shown = await client.showEvent("11111111-1111-4111-8111-111111111111");
expectType<ShowEventEnvelope>(shown);
expectType<string | null | undefined>(shown.event.events[0]!.ctxSessionId);

const located = await client.locateSession({
  provider: "codex",
  providerSession: "codex-fixture-session",
});
expectType<LocationEnvelope<"locateSession">>(located);
expectType<string>(located.location.ctxSessionId);

const envelope = toAgentHistoryEnvelope("search", {
  schema_version: 2,
  query,
  query_execution: {},
  results: [],
});
expectType<SearchEnvelope>(envelope);
expectType<"search">(envelope.operation);
// @ts-expect-error error envelopes are fixture shapes, not local normalization operations.
toAgentHistoryEnvelope("error", {});

function readEnvelope(envelope: AgentHistoryEnvelope): string {
  switch (envelope.operation) {
    case "status":
    case "init":
      return String(envelope.status.initialized);
    case "sources":
      return envelope.sources[0]?.provider ?? "";
    case "import":
    case "sync":
      return String(envelope.import.resume);
    case "search":
      return envelope.search.results[0]?.resultScope ?? "";
    case "showEvent":
      return envelope.event.events[0]?.ctxEventId ?? "";
    case "showSession":
      return envelope.session.events?.[0]?.ctxEventId ?? "";
    case "locateEvent":
    case "locateSession":
      return envelope.location.provider;
    case "error":
      return envelope.error.code;
  }
}
