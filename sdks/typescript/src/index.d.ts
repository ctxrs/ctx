export declare const AGENT_HISTORY_V1_VERSION = "agent-history-v1";
export declare const CTX_SEARCH_V1_VERSION = "ctx-search-v1";
export declare const SDK_VERSION = "0.0.0";

export type Provider =
  | "codex"
  | "pi"
  | "claude"
  | "opencode"
  | "kilo"
  | "kiro-cli"
  | "crush"
  | "goose"
  | "antigravity"
  | "gemini"
  | "tabnine"
  | "cursor"
  | "windsurf"
  | "zed"
  | "copilot-cli"
  | "factory-ai-droid"
  | "qwen-code"
  | "kimi-code-cli"
  | "auggie"
  | "junie"
  | "firebender"
  | "forgecode"
  | "deepagents"
  | "mistral-vibe"
  | "mux"
  | "rovodev"
  | "openclaw"
  | "hermes"
  | "nanoclaw"
  | "astrbot"
  | "shelley"
  | "continue"
  | "openhands"
  | "cline"
  | "roo"
  | "lingma"
  | "mimocode"
  | "qoder"
  | "warp"
  | "codebuddy"
  | "trae"
  | "custom";

export type RefreshMode = "background" | "off" | "wait";
export type ProgressMode = "auto" | "plain" | "json" | "none";
export type TranscriptMode = "lite" | "full" | "log";
export type SearchBackendMode = "hybrid" | "semantic" | "lexical";
export type SearchSemanticReadiness = "ready" | "not_ready" | "unsupported" | "unavailable";
export type SearchEffectiveBackend = "none" | "lexical" | "semantic" | "hybrid";
export type SearchSemanticCompleteness = "not_attempted" | "complete" | "partial" | "skipped";
export type SearchSemanticSkipReason =
  | "disabled"
  | "unavailable"
  | "not_ready"
  | "unsupported"
  | "no_lexical_candidates"
  | "query_shape_not_eligible";

export type SearchAllClause = { all: string };
export type SearchPhraseClause = { phrase: string };
export type SearchLiteralClause = { literal: string };
export type SearchSemanticClause = { semantic: string };
export type SearchLexicalClause = SearchAllClause | SearchPhraseClause | SearchLiteralClause;
export type SearchClause = SearchLexicalClause | SearchSemanticClause;

export interface SearchQueryV1 {
  version: typeof CTX_SEARCH_V1_VERSION;
  any?: SearchClause[];
  must?: SearchLexicalClause[];
  must_not?: SearchLexicalClause[];
}

export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonObject | JsonValue[];
export interface JsonObject {
  [key: string]: JsonValue | undefined;
}

export interface RunRequest {
  command: string;
  args: string[];
  cwd?: string;
  env?: Record<string, string | undefined>;
  timeoutMs?: number;
}

export interface RunResult {
  command?: string;
  args?: string[];
  exitCode?: number | null;
  signal?: string | null;
  stdout?: string;
  stderr?: string;
}

export type Runner = (request: RunRequest) => Promise<RunResult | string> | RunResult | string;

export interface LocalCliAdapterOptions {
  ctxPath?: string;
  dataRoot?: string;
  cwd?: string;
  env?: Record<string, string | undefined>;
  timeoutMs?: number;
  runner?: Runner;
}

export interface LocalAgentHistoryClientOptions extends LocalCliAdapterOptions {
  adapter?: LocalCliAdapter;
}

export interface HostedAgentHistoryClientOptions {
  hosted?: boolean;
  baseUrl?: string;
  apiKey?: string;
}

export interface InitOptions {
  catalogOnly?: boolean;
  progress?: ProgressMode;
}

export interface ImportOptions {
  all?: boolean;
  provider?: Provider;
  path?: string;
  resume?: boolean;
  progress?: ProgressMode;
}

export interface SearchOptions {
  query?: SearchQueryV1;
  limit?: number;
  provider?: Provider;
  historySource?: string;
  providerKey?: string;
  sourceId?: string;
  sourceFormat?: string;
  workspace?: string;
  since?: string;
  primaryOnly?: boolean;
  includeSubagents?: boolean;
  eventType?: string;
  file?: string;
  session?: string;
  events?: boolean;
  backend?: SearchBackendMode;
  refresh?: RefreshMode;
  includeCurrentSession?: boolean;
}

export type SearchIntentOptions = SearchOptions & (
  | { query: SearchQueryV1 }
  | { file: string }
);

export interface ShowEventOptions {
  before?: number;
  after?: number;
  window?: number;
}

export interface SessionLookup {
  id?: string;
  provider?: Provider;
  providerSession?: string;
}

export interface ShowSessionOptions extends SessionLookup {
  mode?: TranscriptMode;
}

export type AgentHistoryOperation =
  | "status"
  | "init"
  | "sources"
  | "import"
  | "sync"
  | "search"
  | "showEvent"
  | "showSession"
  | "locateEvent"
  | "locateSession"
  | "error";

export type ClientAgentHistoryOperation = Exclude<AgentHistoryOperation, "error">;
export type BackendKind = "local" | "hosted";

export interface AgentHistoryBackend {
  kind: BackendKind;
  dataRoot?: string | null;
  baseUrl?: string | null;
}

export interface Totals {
  sourceFiles?: number;
  sourceBytes?: number;
  importedSources?: number;
  failedSources?: number;
  importedSessions?: number;
  importedEvents?: number;
  importedEdges?: number;
  skipped?: number;
  failed?: number;
}

export interface Freshness {
  mode?: string | null;
  status?: string | null;
  reason?: string | null;
  budgetReasons?: string[] | null;
  sourceCount?: number | null;
  daemonLastRunAtMs?: number | null;
  totals?: Totals;
  error?: string | null;
}

export interface AgentHistoryStatus {
  initialized: boolean;
  localOnly: boolean;
  dataRoot?: string | null;
  indexedItems?: number;
  indexedSources?: number;
  catalogedSessions?: number;
  indexedCatalogSessions?: number;
  pendingCatalogSessions?: number;
  failedCatalogSessions?: number;
  staleCatalogSessions?: number;
  freshness?: Freshness;
  semantic?: Record<string, unknown>;
  daemon?: Record<string, unknown>;
}

export interface ProviderSource {
  provider: string;
  path: string;
  exists?: boolean;
  sourceFormat?: string | null;
  status: string;
  importSupport?: string | null;
  nativeImport?: boolean;
  importable: boolean;
  unsupportedReason?: string | null;
}

export interface ImportResult {
  resume: boolean;
  resumeMode?: string | null;
  totals: Totals;
  sources?: JsonObject[];
}

export interface SearchResult {
  schema_version: 2;
  query: SearchQueryV1 | null;
  query_execution: SearchQueryExecution;
  filters?: JsonObject;
  freshness?: Freshness;
  retrieval?: SearchRetrieval;
  generatedAt?: string | null;
  results: SearchHit[];
  pagination?: JsonObject;
  truncation?: JsonObject;
}

export interface SearchExecutionLimits {
  query_bytes: number;
  clauses: number;
  analyzed_tokens_per_clause: number;
  candidates_per_positive_seed: number;
  candidate_rows: number;
  retained_candidate_ids: number;
  residual_rows: number;
  verification_bytes: number;
  verification_lookup_bytes: number;
  hydrated_rows: number;
  hydration_input_bytes: number;
  hydration_input_bytes_per_event: number;
  snippet_input_bytes: number;
  returned_text_bytes: number;
  serialized_response_bytes: number;
  results: number;
  elapsed_ms: number;
}

export interface SearchExecutionConsumption {
  query_bytes: number;
  clauses: number;
  analyzed_tokens: number;
  largest_analyzed_tokens_per_clause: number;
  largest_positive_seed_candidates: number;
  candidate_rows: number;
  retained_candidate_ids: number;
  residual_rows: number;
  verification_bytes: number;
  largest_verification_lookup_bytes: number;
  hydrated_rows: number;
  hydration_input_bytes: number;
  largest_hydration_input_bytes: number;
  snippet_input_bytes: number;
  returned_results: number;
  returned_text_bytes: number;
  serialized_response_bytes: number;
  elapsed_ms: number;
}

export interface SearchSemanticCoverage {
  indexed_documents?: number;
  searchable_documents?: number;
}

export interface SearchSemanticExecution {
  attempted: boolean;
  required: boolean;
  readiness: SearchSemanticReadiness;
  effective_backend: SearchEffectiveBackend;
  backend?: string;
  requested_candidates: number;
  eligible_candidates: number;
  candidates_supplied: number;
  candidates_consumed: number;
  candidates_used: number;
  coverage: SearchSemanticCoverage;
  completeness: SearchSemanticCompleteness;
  incompleteness_reasons?: string[];
  skip_reason?: SearchSemanticSkipReason;
  positive_text_rule_version: string;
}

export interface SearchQueryExecution {
  query_version: typeof CTX_SEARCH_V1_VERSION;
  candidate_strategy: string;
  resolved: SearchExecutionLimits;
  consumed: SearchExecutionConsumption;
  semantic: SearchSemanticExecution;
  rrf_k: number;
  per_branch_candidate_rows: number;
  requested_result_limit: number;
  result_limit: number;
  max_result_limit: number;
  clauses_executed: number;
  verification_dropped: number;
  filter_verification_dropped: number;
  candidate_budget_exhausted: boolean;
  timed_out: boolean;
  truncated: boolean;
  truncation_reasons?: string[];
}

export interface SearchHit {
  ctxEventId?: string | null;
  ctxSessionId?: string | null;
  providerSessionId?: string | null;
  eventSeq?: number | null;
  title?: string | null;
  snippet?: string | null;
  rank?: number | null;
  resultType?: string | null;
  resultScope: string;
  provider?: string | null;
  timestamp?: string | null;
  cwd?: string | null;
  sourcePath?: string | null;
  sourceExists?: boolean | null;
  cursor?: string | null;
  whyMatched?: string[];
  citations?: Citation[];
  suggestedNextCommands?: string[];
  visibility?: string | null;
}

export interface RetrievalCoverage extends JsonObject {
  embeddedItems?: number;
  embeddedChunks?: number;
  searchableItems?: number;
  indexedNow?: number;
  dirtyItems?: number;
}

export interface SearchRetrieval extends JsonObject {
  requestedMode?: SearchBackendMode | string | null;
  effectiveMode?: SearchBackendMode | string | null;
  semanticStatus?: string | null;
  embeddingModel?: string | null;
  coverage?: RetrievalCoverage;
  worker?: JsonObject;
  diagnostics?: JsonObject;
}

export interface Citation {
  itemId?: string | null;
  targetType?: string | null;
  ctxEventId?: string | null;
  ctxSessionId?: string | null;
  label?: string | null;
  time?: string | null;
  provider?: string | null;
  sessionId?: string | null;
  eventSeq?: number | null;
  sourcePath?: string | null;
  sourceExists?: boolean | null;
  cursor?: string | null;
}

export interface AgentHistoryEvent {
  ctxEventId?: string | null;
  ctxSessionId?: string | null;
  sequence?: number | null;
  eventType?: string | null;
  role?: string | null;
  occurredAt?: string | null;
  source?: JsonValue;
  cursor?: string | null;
  text?: string | null;
  preview?: string | null;
  citations?: Citation[];
}

export interface SourceLocation {
  path?: string | null;
  cursor?: string | null;
  exists?: boolean | null;
  sourceId?: string | null;
  sourceFormat?: string | null;
}

export interface EventResult {
  event?: AgentHistoryEvent | null;
  events: AgentHistoryEvent[];
  source?: SourceLocation | null;
}

export interface SessionResult {
  session?: JsonObject | null;
  events?: AgentHistoryEvent[];
  source?: SourceLocation | null;
  mode?: string | null;
  format?: string | null;
}

export interface LocationResult {
  ctxSessionId: string;
  ctxEventId?: string | null;
  provider: string;
  providerSessionId?: string | null;
  source: SourceLocation;
  resume?: JsonObject;
}

export type AgentHistoryErrorCode =
  | "invalid_request"
  | "not_found"
  | "not_initialized"
  | "backend_unavailable"
  | "timeout"
  | "cancelled"
  | "not_supported"
  | "adapter_error"
  | "decode_error"
  | "unknown";

export interface AgentHistoryErrorRecord {
  code: AgentHistoryErrorCode;
  message: string;
  retryable: boolean;
  details?: JsonObject;
  cause?: string | null;
}

export interface AgentHistoryEnvelopeBase<TOperation extends AgentHistoryOperation> {
  contractVersion: typeof AGENT_HISTORY_V1_VERSION;
  schemaVersion: 1;
  operation: TOperation;
  backend?: AgentHistoryBackend;
}

export interface StatusEnvelope extends AgentHistoryEnvelopeBase<"status"> {
  status: AgentHistoryStatus;
}

export interface InitEnvelope extends AgentHistoryEnvelopeBase<"init"> {
  status: AgentHistoryStatus;
}

export interface SourcesEnvelope extends AgentHistoryEnvelopeBase<"sources"> {
  sources: ProviderSource[];
}

export interface ImportEnvelope<TOperation extends "import" | "sync" = "import" | "sync">
  extends AgentHistoryEnvelopeBase<TOperation> {
  import: ImportResult;
}

export interface SearchEnvelope extends AgentHistoryEnvelopeBase<"search"> {
  search: SearchResult;
}

export interface ShowEventEnvelope extends AgentHistoryEnvelopeBase<"showEvent"> {
  event: EventResult;
}

export interface ShowSessionEnvelope extends AgentHistoryEnvelopeBase<"showSession"> {
  session: SessionResult;
}

export interface LocationEnvelope<TOperation extends "locateEvent" | "locateSession">
  extends AgentHistoryEnvelopeBase<TOperation> {
  location: LocationResult;
}

export interface AgentHistoryErrorEnvelope extends AgentHistoryEnvelopeBase<"error"> {
  error: AgentHistoryErrorRecord;
}

export interface AgentHistoryEnvelopeByOperation {
  status: StatusEnvelope;
  init: InitEnvelope;
  sources: SourcesEnvelope;
  import: ImportEnvelope<"import">;
  sync: ImportEnvelope<"sync">;
  search: SearchEnvelope;
  showEvent: ShowEventEnvelope;
  showSession: ShowSessionEnvelope;
  locateEvent: LocationEnvelope<"locateEvent">;
  locateSession: LocationEnvelope<"locateSession">;
  error: AgentHistoryErrorEnvelope;
}

export type AgentHistoryEnvelope = AgentHistoryEnvelopeByOperation[AgentHistoryOperation];

export interface VersionInfo {
  schema_version: 1;
  api_version: typeof AGENT_HISTORY_V1_VERSION;
  sdk_version: typeof SDK_VERSION;
  adapter: "local-cli" | "hosted-placeholder";
  ctx_version?: string;
  hosted?: boolean;
}

export declare class CtxError extends Error {
  code: string;
  details?: unknown;
  constructor(message: string, options?: { code?: string; details?: unknown; cause?: unknown });
}

export declare class CtxCliError extends CtxError {
  exitCode?: number | null;
  signal?: string | null;
  stdout: string;
  stderr: string;
  command?: string;
  args: string[];
}

export declare class CtxParseError extends CtxError {}
export declare class CtxValidationError extends CtxError {}
export declare class CtxUnsupportedError extends CtxError {}
export declare class CtxTimeoutError extends CtxError {}

export declare class LocalCliAdapter {
  ctxPath: string;
  dataRoot?: string;
  cwd?: string;
  env?: Record<string, string | undefined>;
  timeoutMs: number;
  runner?: Runner;
  constructor(options?: LocalCliAdapterOptions);
  execute(
    args: string[],
    options?: Partial<LocalCliAdapterOptions>,
  ): Promise<Required<Pick<RunResult, "stdout" | "stderr">> & RunResult>;
}

export declare class LocalAgentHistoryClient {
  adapter: LocalCliAdapter;
  kind: "local";
  constructor(options?: LocalAgentHistoryClientOptions);
  status(): Promise<StatusEnvelope>;
  init(options?: InitOptions): Promise<InitEnvelope>;
  sources(): Promise<SourcesEnvelope>;
  import(options?: ImportOptions): Promise<ImportEnvelope<"import">>;
  sync(options?: ImportOptions): Promise<ImportEnvelope<"sync">>;
  search(query: SearchQueryV1, options?: Omit<SearchOptions, "query">): Promise<SearchEnvelope>;
  search(options: SearchIntentOptions): Promise<SearchEnvelope>;
  showEvent(id: string, options?: ShowEventOptions): Promise<ShowEventEnvelope>;
  showSession(id: string, options?: Omit<ShowSessionOptions, "id">): Promise<ShowSessionEnvelope>;
  showSession(options: ShowSessionOptions): Promise<ShowSessionEnvelope>;
  locateEvent(id: string): Promise<LocationEnvelope<"locateEvent">>;
  locateSession(id: string): Promise<LocationEnvelope<"locateSession">>;
  locateSession(options: SessionLookup): Promise<LocationEnvelope<"locateSession">>;
  version(): Promise<VersionInfo>;
}

export declare class HostedAgentHistoryClient {
  kind: "hosted";
  baseUrl?: string;
  apiKey?: string;
  constructor(options?: HostedAgentHistoryClientOptions);
  status(): Promise<never>;
  init(): Promise<never>;
  sources(): Promise<never>;
  import(): Promise<never>;
  sync(): Promise<never>;
  search(query: SearchQueryV1, options?: Omit<SearchOptions, "query">): Promise<never>;
  search(options: SearchIntentOptions): Promise<never>;
  showEvent(): Promise<never>;
  showSession(): Promise<never>;
  locateEvent(): Promise<never>;
  locateSession(): Promise<never>;
  version(): Promise<VersionInfo>;
}

export declare function createLocalAgentHistoryClient(options?: LocalAgentHistoryClientOptions): LocalAgentHistoryClient;
export declare function createHostedAgentHistoryClient(options?: HostedAgentHistoryClientOptions): HostedAgentHistoryClient;
export declare function createAgentHistoryClient(
  options?: LocalAgentHistoryClientOptions | HostedAgentHistoryClientOptions,
): LocalAgentHistoryClient | HostedAgentHistoryClient;
export declare function validateSearchQuery(query: SearchQueryV1): SearchQueryV1;
export declare function serializeSearchQuery(query: SearchQueryV1): string;
export declare function toAgentHistoryEnvelope<TOperation extends ClientAgentHistoryOperation>(
  operation: TOperation,
  source: unknown,
  backend?: AgentHistoryBackend,
): AgentHistoryEnvelopeByOperation[TOperation];
