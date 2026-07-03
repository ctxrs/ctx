import {
  type ImportEnvelope,
  type LocationEnvelope,
  type AgentHistoryEnvelope,
  type SearchEnvelope,
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

const search = await client.search("local agent history", { refresh: "off" });
expectType<SearchEnvelope>(search);
expectType<string>(search.search.results[0]!.resultScope);
expectType<string | null | undefined>(search.search.results[0]!.ctxEventId);
// @ts-expect-error search results expose ctxEventId, not ctx_event_id.
search.search.results[0]!.ctx_event_id;

const termSearch = await client.search({ terms: ["local agent history"], refresh: "off" });
expectType<SearchEnvelope>(termSearch);
const fileSearch = await client.search({ file: "src/lib.rs", refresh: "off" });
expectType<SearchEnvelope>(fileSearch);
// @ts-expect-error search requires a query, term, or file option.
await client.search();
// @ts-expect-error search filters alone are not a search intent.
await client.search({ refresh: "off", limit: 5 });

const shown = await client.showEvent("11111111-1111-4111-8111-111111111111");
expectType<ShowEventEnvelope>(shown);
expectType<string | null | undefined>(shown.event.events[0]!.ctxSessionId);

const located = await client.locateSession({
  provider: "codex",
  providerSession: "codex-fixture-session",
});
expectType<LocationEnvelope<"locateSession">>(located);
expectType<string>(located.location.ctxSessionId);

const envelope = toAgentHistoryEnvelope("search", { query: "x", results: [] });
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
