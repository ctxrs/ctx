export declare const AGENT_HISTORY_V1_VERSION = "agent-history-v1";
export declare const SDK_VERSION = "0.0.0";

export type Provider =
  | "codex"
  | "pi"
  | "claude"
  | "opencode"
  | "antigravity"
  | "gemini"
  | "cursor"
  | "copilot-cli"
  | "factory-ai-droid";

export type RefreshMode = "background" | "off" | "wait";
export type ProgressMode = "auto" | "plain" | "json" | "none";
export type TranscriptMode = "lite" | "full" | "log";
export type SearchBackendMode = "hybrid" | "semantic" | "lexical";
export type CoreContentPolicyStatus = "selected" | "redacted" | "omitted";

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
  query?: string;
  term?: string | string[];
  terms?: string[];
  limit?: number;
  provider?: Provider;
  workspace?: string;
  since?: string;
  primaryOnly?: boolean;
  includeSubagents?: boolean;
  eventType?: string;
  file?: string;
  session?: string;
  events?: boolean;
  backend?: SearchBackendMode;
  semanticWeight?: number;
  refresh?: RefreshMode;
  includeCurrentSession?: boolean;
}

export type SearchIntentOptions = SearchOptions & (
  | { query: string }
  | { term: string | string[] }
  | { terms: [string, ...string[]] }
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
  readOnly?: boolean;
  dataRoot?: string | null;
  /** Exact operational counter in the inclusive range 0..Number.MAX_SAFE_INTEGER. */
  indexedItems?: number;
  /** Exact operational counter in the inclusive range 0..Number.MAX_SAFE_INTEGER. */
  indexedSessions?: number;
  /** Exact operational counter in the inclusive range 0..Number.MAX_SAFE_INTEGER. */
  indexedEvents?: number;
  /** Exact operational counter in the inclusive range 0..Number.MAX_SAFE_INTEGER. */
  indexedSources?: number;
  historyEpoch?: Record<string, unknown>;
  lexical?: Record<string, unknown>;
  refresh?: Record<string, unknown>;
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

export interface SearchResultWindow {
  limit: number;
  returned: number;
  moreAvailable: boolean;
}

export interface SearchResult {
  query: string | null;
  filters?: JsonObject;
  freshness?: Freshness;
  retrieval?: SearchRetrieval;
  generatedAt?: string | null;
  results: SearchHit[];
  resultWindow?: SearchResultWindow;
  pagination?: JsonObject;
  truncation?: JsonObject;
}

export interface SearchHit {
  ctxEventId?: string | null;
  ctxSessionId?: string | null;
  providerSessionId?: string | null;
  eventSeq?: number | null;
  title?: string | null;
  snippet?: string | null;
  rank?: number | null;
  retrievalScore?: number | null;
  resultType?: string | null;
  resultScope: string;
  provider?: string | null;
  sourceFormat?: string | null;
  timestamp?: string | null;
  cwd?: string | null;
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
  semanticWeight?: number | null;
  semanticStatus?: string | null;
  semanticFallbackCode?: string | null;
  semanticFallback?: string | null;
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
}

export interface McpToolCall {
  server: string;
  tool: string;
}

export interface AgentHistoryEvent {
  ctxEventId?: string | null;
  ctxSessionId?: string | null;
  provider?: string | null;
  providerSessionId?: string | null;
  sourceFormat?: string | null;
  sequence?: number | null;
  eventType?: string | null;
  role?: string | null;
  occurredAt?: string | null;
  text?: string | null;
  mcpToolCall?: McpToolCall;
  content?: CoreContentMetadata;
  citations?: Citation[];
}

export interface CoreContentMetadata extends JsonObject {
  complete: boolean;
  policyStatus: CoreContentPolicyStatus;
  policyReason?: string | null;
}

export interface SessionSummary extends JsonObject {
  ctxSessionId?: string | null;
  provider?: string | null;
  providerSessionId?: string | null;
  sourceFormat?: string | null;
}

export interface EventResult {
  event?: AgentHistoryEvent | null;
  events: AgentHistoryEvent[];
}

export interface SessionResult {
  session?: SessionSummary | null;
  events?: AgentHistoryEvent[];
  mode?: string | null;
  format?: string | null;
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
  search(query: string, options?: Omit<SearchOptions, "query">): Promise<SearchEnvelope>;
  search(options: SearchIntentOptions): Promise<SearchEnvelope>;
  showEvent(id: string, options?: ShowEventOptions): Promise<ShowEventEnvelope>;
  showSession(id: string, options?: Omit<ShowSessionOptions, "id">): Promise<ShowSessionEnvelope>;
  showSession(options: ShowSessionOptions): Promise<ShowSessionEnvelope>;
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
  search(): Promise<never>;
  showEvent(): Promise<never>;
  showSession(): Promise<never>;
  version(): Promise<VersionInfo>;
}

export declare function createLocalAgentHistoryClient(options?: LocalAgentHistoryClientOptions): LocalAgentHistoryClient;
export declare function createHostedAgentHistoryClient(options?: HostedAgentHistoryClientOptions): HostedAgentHistoryClient;
export declare function createAgentHistoryClient(
  options?: LocalAgentHistoryClientOptions | HostedAgentHistoryClientOptions,
): LocalAgentHistoryClient | HostedAgentHistoryClient;
export declare function toAgentHistoryEnvelope<TOperation extends ClientAgentHistoryOperation>(
  operation: TOperation,
  source: unknown,
  backend?: AgentHistoryBackend,
): AgentHistoryEnvelopeByOperation[TOperation];
