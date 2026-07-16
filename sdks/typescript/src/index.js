import { spawn } from "node:child_process";

export const AGENT_HISTORY_V1_VERSION = "agent-history-v1";
export const CTX_SEARCH_V1_VERSION = "ctx-search-v1";
export const SDK_VERSION = "0.0.0";

const SEARCH_MAX_CLAUSES = 32;
const SEARCH_MAX_CLAUSE_BYTES = 1_024;
const SEARCH_MAX_TOTAL_CLAUSE_BYTES = 8_192;
const SEARCH_MAX_QUERY_JSON_BYTES = 64 * 1_024;
const SEARCH_MAX_ANALYZED_TOKENS_PER_CLAUSE = 32;
const SEARCH_MIN_LITERAL_BYTES = 3;
const SEARCH_MAX_LITERAL_BYTES = 256;
const SEARCH_MAX_RESULTS = 200;
const SEARCH_QUERY_FIELDS = new Set(["version", "any", "must", "must_not"]);
const SEARCH_LEXICAL_MATCHERS = new Set(["all", "phrase", "literal"]);
const SEARCH_ANY_MATCHERS = new Set([...SEARCH_LEXICAL_MATCHERS, "semantic"]);
const SEARCH_ALPHANUMERIC = /^[\p{Alphabetic}\p{Number}]$/u;

export class CtxError extends Error {
  constructor(message, options = {}) {
    super(message, options.cause ? { cause: options.cause } : undefined);
    this.name = "CtxError";
    this.code = options.code ?? "CTX_ERROR";
    this.details = options.details;
  }
}

export class CtxCliError extends CtxError {
  constructor(message, options = {}) {
    super(message, {
      code: options.code ?? "CTX_CLI_ERROR",
      details: {
        command: options.command,
        args: options.args,
        exitCode: options.exitCode,
        signal: options.signal,
        stdout: options.stdout,
        stderr: options.stderr,
        ...options.details,
      },
      cause: options.cause,
    });
    this.name = "CtxCliError";
    this.exitCode = options.exitCode;
    this.signal = options.signal;
    this.stdout = options.stdout ?? "";
    this.stderr = options.stderr ?? "";
    this.command = options.command;
    this.args = options.args ?? [];
  }
}

export class CtxParseError extends CtxError {
  constructor(message, options = {}) {
    super(message, {
      code: options.code ?? "CTX_PARSE_ERROR",
      details: options.details,
      cause: options.cause,
    });
    this.name = "CtxParseError";
  }
}

export class CtxValidationError extends CtxError {
  constructor(message, options = {}) {
    super(message, {
      code: options.code ?? "CTX_VALIDATION_ERROR",
      details: options.details,
      cause: options.cause,
    });
    this.name = "CtxValidationError";
  }
}

export class CtxUnsupportedError extends CtxError {
  constructor(message, options = {}) {
    super(message, {
      code: options.code ?? "CTX_UNSUPPORTED",
      details: options.details,
      cause: options.cause,
    });
    this.name = "CtxUnsupportedError";
  }
}

export class CtxTimeoutError extends CtxError {
  constructor(message, options = {}) {
    super(message, {
      code: options.code ?? "timeout",
      details: options.details,
      cause: options.cause,
    });
    this.name = "CtxTimeoutError";
  }
}

export class LocalCliAdapter {
  constructor(options = {}) {
    this.ctxPath = options.ctxPath ?? "ctx";
    this.dataRoot = options.dataRoot;
    this.cwd = options.cwd;
    this.env = options.env;
    this.timeoutMs = options.timeoutMs ?? 60_000;
    this.runner = options.runner;
  }

  async execute(args, options = {}) {
    const argv = this.#argv(args);
    const command = this.ctxPath;
    if (this.runner) {
      return normalizeRunResult(
        await this.runner({
          command,
          args: argv,
          cwd: options.cwd ?? this.cwd,
          env: { ...this.env, ...options.env },
          timeoutMs: options.timeoutMs ?? this.timeoutMs,
        }),
        command,
        argv,
      );
    }
    return spawnCommand(command, argv, {
      cwd: options.cwd ?? this.cwd,
      env: { ...process.env, ...this.env, ...options.env },
      timeoutMs: options.timeoutMs ?? this.timeoutMs,
    });
  }

  #argv(args) {
    const argv = [];
    if (this.dataRoot) {
      argv.push("--data-root", String(this.dataRoot));
    }
    argv.push(...args.map(String));
    return argv;
  }
}

export class LocalAgentHistoryClient {
  constructor(options = {}) {
    this.adapter = options.adapter ?? new LocalCliAdapter(options);
    this.kind = "local";
  }

  async status() {
    return this.#agentHistoryJson("status", ["status", "--json"]);
  }

  async init(options = {}) {
    const args = ["setup", "--json", "--progress", options.progress ?? "none"];
    if (options.catalogOnly) {
      args.push("--catalog-only");
    }
    return this.#agentHistoryJson("init", args);
  }

  async sources() {
    return this.#agentHistoryJson("sources", ["sources", "--json"]);
  }

  async import(options = {}) {
    const args = ["import", "--json", "--progress", options.progress ?? "none"];
    appendImportArgs(args, options);
    return this.#agentHistoryJson("import", args);
  }

  async sync(options = {}) {
    const args = ["import", "--json", "--progress", options.progress ?? "none"];
    appendImportArgs(args, options);
    return this.#agentHistoryJson("sync", args);
  }

  async search(queryOrOptions = undefined, maybeOptions = {}) {
    const options = searchCallOptions(queryOrOptions, maybeOptions);
    validateSearchIntent(options);
    const args = ["search"];
    if (options.query) {
      args.push("--query-json", serializeSearchQuery(options.query));
    }
    appendSearchArgs(args, options);
    args.push("--json");
    return this.#agentHistoryJson("search", args);
  }

  async showEvent(id, options = {}) {
    requireId("event id", id);
    const args = ["show", "event", id, "--format", "json"];
    appendOptionalNumber(args, "--before", options.before);
    appendOptionalNumber(args, "--after", options.after);
    appendOptionalNumber(args, "--window", options.window);
    return this.#agentHistoryJson("showEvent", args);
  }

  async showSession(idOrOptions, maybeOptions = {}) {
    const options =
      typeof idOrOptions === "string"
        ? { ...maybeOptions, id: idOrOptions }
        : { ...idOrOptions };
    const args = ["show", "session"];
    appendSessionLookupArgs(args, options);
    args.push("--mode", options.mode ?? "lite", "--format", "json");
    return this.#agentHistoryJson("showSession", args);
  }

  async locateEvent(id) {
    requireId("event id", id);
    return this.#agentHistoryJson("locateEvent", ["locate", "event", id, "--format", "json"]);
  }

  async locateSession(idOrOptions) {
    const options =
      typeof idOrOptions === "string" ? { id: idOrOptions } : { ...idOrOptions };
    const args = ["locate", "session"];
    appendSessionLookupArgs(args, options);
    args.push("--format", "json");
    return this.#agentHistoryJson("locateSession", args);
  }

  async version() {
    const result = await this.adapter.execute(["--version"]);
    if (result.exitCode !== 0) {
      throw cliError("ctx --version failed", result);
    }
    const raw = result.stdout.trim();
    return {
      schema_version: 1,
      api_version: AGENT_HISTORY_V1_VERSION,
      sdk_version: SDK_VERSION,
      adapter: "local-cli",
      ctx_version: parseCtxVersion(raw),
    };
  }

  async #agentHistoryJson(operation, args) {
    return toAgentHistoryEnvelope(operation, await this.#json(args), {
      kind: "local",
      dataRoot: this.adapter.dataRoot ?? null,
    });
  }

  async #json(args) {
    const result = await this.adapter.execute(args);
    if (result.exitCode !== 0) {
      throw cliError(`ctx ${args.join(" ")} failed`, result);
    }
    try {
      return JSON.parse(result.stdout);
    } catch (cause) {
      throw new CtxParseError("ctx returned invalid JSON", {
        details: {
          command: result.command,
          args: result.args,
          stdout: result.stdout,
          stderr: result.stderr,
        },
        cause,
      });
    }
  }
}

export class HostedAgentHistoryClient {
  constructor(options = {}) {
    this.kind = "hosted";
    this.baseUrl = options.baseUrl;
    this.apiKey = options.apiKey;
  }

  status() {
    return hostedUnsupported();
  }

  init() {
    return hostedUnsupported();
  }

  sources() {
    return hostedUnsupported();
  }

  import() {
    return hostedUnsupported();
  }

  sync() {
    return hostedUnsupported();
  }

  async search(queryOrOptions = undefined, maybeOptions = {}) {
    const options = searchCallOptions(queryOrOptions, maybeOptions);
    validateSearchIntent(options);
    if (options.query) {
      serializeSearchQuery(options.query);
    }
    return hostedUnsupported();
  }

  showEvent() {
    return hostedUnsupported();
  }

  showSession() {
    return hostedUnsupported();
  }

  locateEvent() {
    return hostedUnsupported();
  }

  locateSession() {
    return hostedUnsupported();
  }

  version() {
    return Promise.resolve({
      schema_version: 1,
      api_version: AGENT_HISTORY_V1_VERSION,
      sdk_version: SDK_VERSION,
      adapter: "hosted-placeholder",
      hosted: false,
    });
  }
}

export function createLocalAgentHistoryClient(options = {}) {
  return new LocalAgentHistoryClient(options);
}

export function createHostedAgentHistoryClient(options = {}) {
  return new HostedAgentHistoryClient(options);
}

export function createAgentHistoryClient(options = {}) {
  if (options.hosted || options.baseUrl) {
    return createHostedAgentHistoryClient(options);
  }
  return createLocalAgentHistoryClient(options);
}

function hostedUnsupported() {
  return Promise.reject(
    new CtxUnsupportedError(
      "The hosted agent-history-v1 transport is reserved for future ctx service support. Use the local CLI adapter today.",
      { details: { adapter: "hosted-placeholder" } },
    ),
  );
}

export function toAgentHistoryEnvelope(operation, source, backend = undefined) {
  const envelope = {
    contractVersion: AGENT_HISTORY_V1_VERSION,
    schemaVersion: 1,
    operation,
    ...(backend ? { backend } : {}),
  };
  const raw = source;
  switch (operation) {
    case "status":
    case "init":
      envelope.status = camelizeKeys(raw);
      break;
    case "sources":
      envelope.sources = camelizeKeys(raw?.sources ?? []);
      break;
    case "import":
    case "sync":
      envelope.import = camelizeKeys(raw);
      break;
    case "search":
      envelope.search = normalizeSearchResponse(raw);
      break;
    case "showEvent":
      envelope.event = {
        event: camelizeKeys(raw?.event ?? null),
        events: camelizeKeys(raw?.events ?? []),
        source: camelizeKeys(raw?.source ?? null),
      };
      break;
    case "showSession":
      envelope.session = {
        session: camelizeKeys(raw?.session ?? null),
        events: camelizeKeys(raw?.events ?? []),
        source: camelizeKeys(raw?.source ?? null),
        mode: camelizeKeys(raw?.mode ?? null),
        format: camelizeKeys(raw?.format ?? null),
      };
      break;
    case "locateEvent":
    case "locateSession":
      envelope.location = camelizeKeys(raw);
      break;
    default:
      throw new CtxValidationError(`unsupported agent-history-v1 operation: ${operation}`, {
        details: { operation },
      });
  }
  return envelope;
}

function camelizeKeys(value) {
  if (Array.isArray(value)) {
    return value.map((item) => camelizeKeys(item));
  }
  if (!value || typeof value !== "object") {
    return value;
  }
  const out = {};
  for (const [key, item] of Object.entries(value)) {
    const camelKey = key.replace(/_([a-z])/g, (_, char) => char.toUpperCase());
    if (
      camelKey === "databasePath" ||
      camelKey === "configPath" ||
      camelKey === "itemType" ||
      camelKey === "payloadType" ||
      camelKey === "recordType" ||
      camelKey === "semanticWeight" ||
      camelKey === "semanticFallbackCode" ||
      camelKey === "semanticFallback"
    ) {
      continue;
    }
    out[camelKey] = camelizeKeys(item);
  }
  return out;
}

function appendImportArgs(args, options) {
  if (options.all) {
    args.push("--all");
  }
  if (options.provider) {
    args.push("--provider", options.provider);
  }
  if (options.path) {
    args.push("--path", options.path);
  }
  if (options.resume) {
    args.push("--resume");
  }
}

function appendSearchArgs(args, options) {
  appendOptionalNumber(args, "--limit", options.limit);
  appendOptional(args, "--provider", options.provider);
  appendOptional(args, "--history-source", options.historySource);
  appendOptional(args, "--provider-key", options.providerKey);
  appendOptional(args, "--source-id", options.sourceId);
  appendOptional(args, "--source-format", options.sourceFormat);
  appendOptional(args, "--workspace", options.workspace);
  appendOptional(args, "--since", options.since);
  appendFlag(args, "--primary-only", options.primaryOnly);
  appendFlag(args, "--include-subagents", options.includeSubagents);
  appendOptional(args, "--event-type", options.eventType);
  appendOptional(args, "--file", options.file);
  appendOptional(args, "--session", options.session);
  appendFlag(args, "--events", options.events);
  appendOptional(args, "--backend", options.backend);
  appendOptional(args, "--refresh", options.refresh);
  appendFlag(args, "--include-current-session", options.includeCurrentSession);
}

function validateSearchIntent(options) {
  validateSearchLimit(options.limit);
  if (options.query !== undefined) {
    validateSearchQuery(options.query);
    return;
  }
  if (hasSearchText(options.file)) {
    return;
  }
  throw new CtxValidationError("search requires a ctx-search-v1 query or file option", {
    details: { options },
  });
}

function searchCallOptions(queryOrOptions, maybeOptions) {
  if (looksLikeSearchQuery(queryOrOptions)) {
    return { ...maybeOptions, query: queryOrOptions };
  }
  if (queryOrOptions === undefined) {
    return {};
  }
  if (isObject(queryOrOptions)) {
    return { ...queryOrOptions };
  }
  throw new CtxValidationError("search input must be a ctx-search-v1 query or options object", {
    details: { inputType: typeof queryOrOptions },
  });
}

function hasSearchText(value) {
  return typeof value === "string" && value.trim().length > 0;
}

function appendSessionLookupArgs(args, options) {
  if (options.id) {
    args.push(options.id);
    return;
  }
  appendOptional(args, "--provider", options.provider);
  appendOptional(args, "--provider-session", options.providerSession);
  if (!options.provider || !options.providerSession) {
    throw new CtxValidationError(
      "session lookup requires either id or provider with providerSession",
      { details: { options } },
    );
  }
}

export function serializeSearchQuery(query) {
  const canonical = validateSearchQuery(query);
  const serialized = JSON.stringify(canonical);
  const encodedBytes = Buffer.byteLength(serialized, "utf8");
  if (encodedBytes > SEARCH_MAX_QUERY_JSON_BYTES) {
    invalidSearchQuery("search query JSON exceeds the 65536-byte limit", {
      actualBytes: encodedBytes,
      maximumBytes: SEARCH_MAX_QUERY_JSON_BYTES,
    });
  }
  return serialized;
}

export function validateSearchQuery(query) {
  if (!isObject(query)) {
    invalidSearchQuery("search query must be an object", { queryType: typeof query });
  }
  const unknown = firstUnknownOwnField(query, SEARCH_QUERY_FIELDS);
  if (unknown !== undefined) {
    invalidSearchQuery("search query contains an unknown field", { field: unknown });
  }
  if (query.version !== CTX_SEARCH_V1_VERSION) {
    invalidSearchQuery("search query version must be ctx-search-v1", {
      version: query.version,
    });
  }

  const canonical = { version: CTX_SEARCH_V1_VERSION };
  const canonicalPlacements = {};
  for (const placement of ["any", "must", "must_not"]) {
    const rawClauses = query[placement] ?? [];
    if (!Array.isArray(rawClauses)) {
      invalidSearchQuery(`search query ${placement} must be an array`, { placement });
    }
    const allowed = placement === "any" ? SEARCH_ANY_MATCHERS : SEARCH_LEXICAL_MATCHERS;
    const clauses = [];
    const seen = new Set();
    for (const rawClause of rawClauses) {
      if (!isObject(rawClause)) {
        invalidSearchQuery("search clause must be an object", { placement });
      }
      const matchers = firstOwnFields(rawClause, 2);
      if (matchers.length !== 1 || !allowed.has(matchers[0])) {
        invalidSearchQuery("search clause must contain exactly one allowed matcher", {
          placement,
          matchers,
        });
      }
      const matcher = matchers[0];
      const value = rawClause[matcher];
      if (typeof value !== "string") {
        invalidSearchQuery("search clause value must be a string", {
          placement,
          matcher,
        });
      }
      const canonicalValue =
        matcher === "literal" ? value.trim() : (value.match(/\S+/gu) ?? []).join(" ");
      const identity = JSON.stringify([matcher, canonicalValue]);
      if (seen.has(identity)) {
        continue;
      }
      seen.add(identity);
      clauses.push({ [matcher]: canonicalValue });
    }
    if (clauses.length > 0) {
      canonical[placement] = clauses;
      canonicalPlacements[placement] = clauses;
    }
  }

  const positiveClauses =
    (canonicalPlacements.any?.length ?? 0) + (canonicalPlacements.must?.length ?? 0);
  if (positiveClauses === 0) {
    invalidSearchQuery("search query needs a positive any or must clause", {});
  }

  const allClauses = [
    ...(canonicalPlacements.any ?? []),
    ...(canonicalPlacements.must ?? []),
    ...(canonicalPlacements.must_not ?? []),
  ];
  if (allClauses.length > SEARCH_MAX_CLAUSES) {
    invalidSearchQuery("search query exceeds the 32-clause limit", {
      actualClauses: allClauses.length,
      maximumClauses: SEARCH_MAX_CLAUSES,
    });
  }

  const semanticClauses = (canonicalPlacements.any ?? []).filter((clause) =>
    Object.hasOwn(clause, "semantic"),
  ).length;
  if (semanticClauses > 1) {
    invalidSearchQuery("search query allows at most one semantic clause in any", {});
  }

  let totalClauseBytes = 0;
  for (const clause of allClauses) {
    const [matcher] = Object.keys(clause);
    const value = clause[matcher];
    const valueBytes = Buffer.byteLength(value, "utf8");
    if (valueBytes === 0) {
      invalidSearchQuery("search clause cannot be empty", { matcher });
    }
    if (valueBytes > SEARCH_MAX_CLAUSE_BYTES) {
      invalidSearchQuery("search clause exceeds the 1024-byte limit", {
        matcher,
        actualBytes: valueBytes,
      });
    }
    if (
      matcher === "literal" &&
      (valueBytes < SEARCH_MIN_LITERAL_BYTES || valueBytes > SEARCH_MAX_LITERAL_BYTES)
    ) {
      invalidSearchQuery("literal search clause must be between 3 and 256 bytes", {
        actualBytes: valueBytes,
      });
    }
    const analyzedTokens = searchAnalyzedTokenCount(value);
    if (analyzedTokens === 0) {
      invalidSearchQuery("search clause has no searchable tokens", { matcher });
    }
    if (analyzedTokens > SEARCH_MAX_ANALYZED_TOKENS_PER_CLAUSE) {
      invalidSearchQuery("search clause exceeds the 32 analyzed-token limit", {
        matcher,
        actualTokens: analyzedTokens,
        maximumTokens: SEARCH_MAX_ANALYZED_TOKENS_PER_CLAUSE,
      });
    }
    totalClauseBytes += valueBytes;
  }
  if (totalClauseBytes > SEARCH_MAX_TOTAL_CLAUSE_BYTES) {
    invalidSearchQuery("search query exceeds the 8192-byte clause limit", {
      actualBytes: totalClauseBytes,
      maximumBytes: SEARCH_MAX_TOTAL_CLAUSE_BYTES,
    });
  }
  return canonical;
}

function validateSearchLimit(limit) {
  if (limit === undefined) {
    return;
  }
  if (!Number.isInteger(limit) || limit < 1 || limit > SEARCH_MAX_RESULTS) {
    throw new CtxValidationError("search limit must be an integer between 1 and 200", {
      details: { limit, minimum: 1, maximum: SEARCH_MAX_RESULTS },
    });
  }
}

function searchAnalyzedTokenCount(value) {
  let count = 0;
  let inToken = false;
  for (const char of value) {
    const continuesToken =
      SEARCH_ALPHANUMERIC.test(char) || (inToken && isSearchContinuationMark(char));
    if (continuesToken) {
      if (!inToken) {
        count += 1;
      }
      inToken = true;
    } else {
      inToken = false;
    }
  }
  return count;
}

function isSearchContinuationMark(char) {
  const codepoint = char.codePointAt(0);
  return (
    (codepoint >= 0x0300 && codepoint <= 0x036f) ||
    (codepoint >= 0x1ab0 && codepoint <= 0x1aff) ||
    (codepoint >= 0x1dc0 && codepoint <= 0x1dff) ||
    (codepoint >= 0x20d0 && codepoint <= 0x20ff) ||
    (codepoint >= 0xfe20 && codepoint <= 0xfe2f) ||
    codepoint === 0x200c ||
    codepoint === 0x200d
  );
}

function normalizeSearchResponse(raw) {
  const schemaVersion = raw?.schema_version;
  if (schemaVersion !== 2) {
    throw new CtxParseError("ctx search returned an unsupported schema version", {
      details: { expectedSchemaVersion: 2, actualSchemaVersion: schemaVersion },
    });
  }
  if (raw.query !== null && raw.query !== undefined && !isObject(raw.query)) {
    throw new CtxParseError("ctx search response contains a non-object canonical query", {
      details: { field: "query" },
    });
  }
  let query = null;
  if (isObject(raw.query)) {
    try {
      query = validateSearchQuery(raw.query);
    } catch (cause) {
      throw new CtxParseError("ctx search returned an invalid canonical query", {
        details: { field: "query" },
        cause,
      });
    }
  }
  const queryExecution = raw.query_execution;
  if (!isObject(queryExecution)) {
    throw new CtxParseError("ctx search response is missing query execution diagnostics", {
      details: { field: "query_execution" },
    });
  }
  const { schema_version, query: _query, query_execution, ...legacyFields } = raw;
  return {
    ...camelizeKeys(legacyFields),
    schema_version,
    query,
    query_execution: queryExecution,
  };
}

function looksLikeSearchQuery(value) {
  return (
    isObject(value) &&
    ["version", "any", "must", "must_not"].some((field) => Object.hasOwn(value, field))
  );
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function firstUnknownOwnField(value, allowed) {
  for (const field in value) {
    if (Object.hasOwn(value, field) && !allowed.has(field)) {
      return field;
    }
  }
  return undefined;
}

function firstOwnFields(value, limit) {
  const fields = [];
  for (const field in value) {
    if (Object.hasOwn(value, field)) {
      fields.push(field);
      if (fields.length >= limit) {
        break;
      }
    }
  }
  return fields;
}

function invalidSearchQuery(message, details) {
  throw new CtxValidationError(message, { details });
}

function appendOptional(args, flag, value) {
  if (value !== undefined && value !== null && value !== false) {
    args.push(flag, value);
  }
}

function appendOptionalNumber(args, flag, value) {
  if (value !== undefined && value !== null) {
    args.push(flag, String(value));
  }
}

function appendFlag(args, flag, value) {
  if (value) {
    args.push(flag);
  }
}

function requireId(label, id) {
  if (!id || typeof id !== "string") {
    throw new CtxValidationError(`${label} is required`, {
      details: { value: id },
    });
  }
}

function cliError(message, result) {
  return new CtxCliError(message, {
    command: result.command,
    args: result.args,
    exitCode: result.exitCode,
    signal: result.signal,
    stdout: result.stdout,
    stderr: result.stderr,
  });
}

function normalizeRunResult(result, command, args) {
  if (typeof result === "string") {
    return { command, args, exitCode: 0, stdout: result, stderr: "" };
  }
  return {
    command: result.command ?? command,
    args: result.args ?? args,
    exitCode: result.exitCode ?? 0,
    signal: result.signal,
    stdout: result.stdout ?? "",
    stderr: result.stderr ?? "",
  };
}

function spawnCommand(command, args, options) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: options.cwd,
      env: options.env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    let settled = false;
    const timeout = setTimeout(() => {
      settled = "timeout";
      child.kill("SIGTERM");
    }, options.timeoutMs);

    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.on("error", (cause) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timeout);
      reject(
        new CtxCliError(`failed to start ${command}`, {
          command,
          args,
          exitCode: undefined,
          stdout,
          stderr,
          cause,
        }),
      );
    });
    child.on("close", (exitCode, signal) => {
      if (settled === true) {
        return;
      }
      if (settled === "timeout") {
        settled = true;
        clearTimeout(timeout);
        reject(
          new CtxTimeoutError(`ctx command timed out after ${options.timeoutMs}ms`, {
            details: { command, args, exitCode, signal, stdout, stderr, timeoutMs: options.timeoutMs },
          }),
        );
        return;
      }
      settled = true;
      clearTimeout(timeout);
      resolve({ command, args, exitCode, signal, stdout, stderr });
    });
  });
}

function parseCtxVersion(raw) {
  const match = raw.match(/^ctx\s+(.+)$/);
  return match ? match[1] : raw || undefined;
}
