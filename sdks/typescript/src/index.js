import { spawnCommand } from "./subprocess.js";

export const AGENT_HISTORY_V1_VERSION = "agent-history-v1";
export const SDK_VERSION = "0.0.0";

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
    const env = {
      ...this.env,
      ...options.env,
      CTX_ANALYTICS_ENABLED: "false",
    };
    const result = this.runner
      ? await this.runner({
          command,
          args: argv,
          cwd: options.cwd ?? this.cwd,
          env,
          timeoutMs: options.timeoutMs ?? this.timeoutMs,
        })
      : await spawnCommand(
          command,
          argv,
          {
            cwd: options.cwd ?? this.cwd,
            env: { ...process.env, ...env },
            timeoutMs: options.timeoutMs ?? this.timeoutMs,
          },
          { CtxCliError, CtxParseError, CtxTimeoutError },
        );
    return normalizeRunResult(result, command, argv);
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
    return this.#agentHistoryJson("status", ["status", "--format=json"]);
  }

  async init(options = {}) {
    const args = ["setup", "--format=json", "--progress", options.progress ?? "none"];
    return this.#agentHistoryJson("init", args);
  }

  async sources() {
    return this.#agentHistoryJson("sources", ["sources", "--format=json"]);
  }

  async import(options = {}) {
    const args = ["import", "--format=json", "--progress", options.progress ?? "none"];
    appendImportArgs(args, options);
    return this.#agentHistoryJson("import", args);
  }

  async sync(options = {}) {
    const args = ["import", "--format=json", "--progress", options.progress ?? "none"];
    appendImportArgs(args, options);
    return this.#agentHistoryJson("sync", args);
  }

  async search(queryOrOptions = undefined, maybeOptions = {}) {
    const options =
      typeof queryOrOptions === "string"
        ? { ...maybeOptions, query: queryOrOptions }
        : { ...queryOrOptions };
    validateSearchOptions(options);
    const args = ["search"];
    if (options.query) {
      args.push(options.query);
    }
    appendSearchArgs(args, options);
    args.push("--format=json");
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
    const validatesStatusCounters = operation === "status" || operation === "init";
    return toAgentHistoryEnvelope(operation, await this.#json(args, validatesStatusCounters), {
      kind: "local",
      dataRoot: this.adapter.dataRoot ?? null,
    });
  }

  async #json(args, validatesStatusCounters = false) {
    const result = await this.adapter.execute(args);
    if (result.exitCode !== 0) {
      throw cliError(`ctx ${args.join(" ")} failed`, result);
    }
    try {
      if (validatesStatusCounters) {
        validateExactStatusCounterLexemes(result.stdout);
      }
      validateNoDuplicateJSONMembers(result.stdout);
      return JSON.parse(result.stdout);
    } catch (cause) {
      if (cause instanceof CtxParseError) {
        throw cause;
      }
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

const STATUS_COUNTER_WIRE_KEYS = new Map([
  ["indexed_items", "indexedItems"],
  ["indexed_sessions", "indexedSessions"],
  ["indexed_events", "indexedEvents"],
  ["indexed_sources", "indexedSources"],
  ["indexedItems", "indexedItems"],
  ["indexedSessions", "indexedSessions"],
  ["indexedEvents", "indexedEvents"],
  ["indexedSources", "indexedSources"],
]);

function validateExactStatusCounterLexemes(json) {
  let index = 0;
  let depth = 0;
  while (index < json.length) {
    if (json[index] === "{" || json[index] === "[") {
      depth += 1;
      index += 1;
      continue;
    }
    if (json[index] === "}" || json[index] === "]") {
      depth -= 1;
      index += 1;
      continue;
    }
    if (json[index] !== '"') {
      index += 1;
      continue;
    }
    const stringStart = index;
    index += 1;
    while (index < json.length) {
      if (json[index] === "\\") {
        index += 2;
      } else if (json[index] === '"') {
        index += 1;
        break;
      } else {
        index += 1;
      }
    }
    if (depth !== 1) continue;
    let cursor = index;
    while (/\s/u.test(json[cursor] ?? "")) cursor += 1;
    if (json[cursor] !== ":") continue;
    let key;
    try {
      key = JSON.parse(json.slice(stringStart, index));
    } catch {
      continue;
    }
    const field = STATUS_COUNTER_WIRE_KEYS.get(key);
    if (!field) continue;
    cursor += 1;
    while (/\s/u.test(json[cursor] ?? "")) cursor += 1;
    const number = /^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/u.exec(
      json.slice(cursor),
    )?.[0];
    if (number && !/^(?:0|[1-9]\d*)$/u.test(number)) {
      throw new CtxParseError(
        `ctx status counter ${field} is outside the exact JSON integer domain`,
        { details: { field, maximum: Number.MAX_SAFE_INTEGER } },
      );
    }
  }
}

function validateNoDuplicateJSONMembers(json) {
  let index = 0;

  const fail = (message) => {
    throw new CtxParseError(`ctx returned invalid JSON: ${message}`, {
      details: { offset: index },
    });
  };
  const skipWhitespace = () => {
    while (json[index] === " " || json[index] === "\n" || json[index] === "\r" || json[index] === "\t") {
      index += 1;
    }
  };
  const parseString = () => {
    if (json[index] !== '"') fail("expected string");
    const start = index;
    index += 1;
    while (index < json.length) {
      const character = json[index];
      index += 1;
      if (character === '"') {
        try {
          return JSON.parse(json.slice(start, index));
        } catch {
          fail("invalid string");
        }
      }
      if (character === "\\") {
        if (index >= json.length) fail("unterminated escape");
        index += 1;
      }
    }
    fail("unterminated string");
  };
  const parseValue = () => {
    skipWhitespace();
    if (json[index] === "{") {
      parseObject();
      return;
    }
    if (json[index] === "[") {
      parseArray();
      return;
    }
    if (json[index] === '"') {
      parseString();
      return;
    }
    const start = index;
    while (
      index < json.length &&
      json[index] !== "," &&
      json[index] !== "}" &&
      json[index] !== "]" &&
      !/\s/u.test(json[index])
    ) {
      index += 1;
    }
    if (start === index) fail("expected value");
  };
  const parseObject = () => {
    index += 1;
    const members = new Set();
    skipWhitespace();
    if (json[index] === "}") {
      index += 1;
      return;
    }
    while (index < json.length) {
      skipWhitespace();
      const member = parseString();
      if (members.has(member)) {
        throw new CtxParseError("ctx JSON contains a duplicate object member", {
          details: { member },
        });
      }
      members.add(member);
      skipWhitespace();
      if (json[index] !== ":") fail("expected colon");
      index += 1;
      parseValue();
      skipWhitespace();
      if (json[index] === "}") {
        index += 1;
        return;
      }
      if (json[index] !== ",") fail("expected comma");
      index += 1;
    }
    fail("unterminated object");
  };
  const parseArray = () => {
    index += 1;
    skipWhitespace();
    if (json[index] === "]") {
      index += 1;
      return;
    }
    while (index < json.length) {
      parseValue();
      skipWhitespace();
      if (json[index] === "]") {
        index += 1;
        return;
      }
      if (json[index] !== ",") fail("expected comma");
      index += 1;
    }
    fail("unterminated array");
  };

  parseValue();
  skipWhitespace();
  if (index !== json.length) fail("trailing data");
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

  search() {
    return hostedUnsupported();
  }

  showEvent() {
    return hostedUnsupported();
  }

  showSession() {
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
      envelope.status = normalizeStatus(raw);
      break;
    case "sources":
      envelope.sources = camelizeKeys(raw?.sources ?? []);
      break;
    case "import":
    case "sync":
      envelope.import = camelizeKeys(raw);
      break;
    case "search": {
      const search = camelizeKeys(raw);
      bridgeSearchPagination(search);
      envelope.search = search;
      break;
    }
    case "showEvent":
      envelope.event = {
        event: normalizeEventRecord(raw?.event ?? null),
        events: normalizeEventRecords(raw?.events ?? []),
      };
      break;
    case "showSession":
      envelope.session = {
        session: camelizeKeys(raw?.session ?? null),
        events: normalizeEventRecords(raw?.events ?? []),
        mode: camelizeKeys(raw?.mode ?? null),
        format: camelizeKeys(raw?.format ?? null),
      };
      break;
    default:
      throw new CtxValidationError(`unsupported agent-history-v1 operation: ${operation}`, {
        details: { operation },
      });
  }
  return envelope;
}

const MAX_MCP_TOOL_CALL_COMPONENT_BYTES = 64 * 1024;

function normalizeEventRecords(values) {
  return Array.isArray(values)
    ? values.map((value) => normalizeEventRecord(value))
    : camelizeKeys(values);
}

function normalizeEventRecord(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return camelizeKeys(value);
  }

  const hasSnake = Object.hasOwn(value, "mcp_tool_call");
  const hasCamel = Object.hasOwn(value, "mcpToolCall");
  if (hasSnake && hasCamel) {
    throw invalidMcpToolCall("duplicate outer wire aliases");
  }

  const outer = {};
  for (const [key, item] of Object.entries(value)) {
    if (key === "mcp_tool_call" || key === "mcpToolCall") {
      continue;
    }
    if (snakeToCamel(key) === "mcpToolCall") {
      throw invalidMcpToolCall("outer member collides with the canonical mcpToolCall key", {
        member: key,
      });
    }
    outer[key] = item;
  }

  const event = camelizeKeys(outer);
  if (hasSnake || hasCamel) {
    event.mcpToolCall = validateMcpToolCall(
      hasSnake ? value.mcp_tool_call : value.mcpToolCall,
    );
  }
  return event;
}

function validateMcpToolCall(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw invalidMcpToolCall("expected an object");
  }
  const members = Object.keys(value).sort();
  if (members.length !== 2 || members[0] !== "server" || members[1] !== "tool") {
    throw invalidMcpToolCall("expected exactly server and tool", { members });
  }
  return {
    server: validateMcpToolCallComponent(value.server, "server"),
    tool: validateMcpToolCallComponent(value.tool, "tool"),
  };
}

function validateMcpToolCallComponent(value, field) {
  if (typeof value !== "string") {
    throw invalidMcpToolCall("expected a string", { field: `mcpToolCall.${field}` });
  }
  let decodedBytes = 0;
  for (let index = 0; index < value.length; index += 1) {
    const codeUnit = value.charCodeAt(index);
    if (codeUnit >= 0xd800 && codeUnit <= 0xdbff) {
      const low = value.charCodeAt(index + 1);
      if (!(low >= 0xdc00 && low <= 0xdfff)) {
        throw invalidMcpToolCall("contains an invalid Unicode string", {
          field: `mcpToolCall.${field}`,
        });
      }
      decodedBytes += 4;
      index += 1;
    } else if (codeUnit >= 0xdc00 && codeUnit <= 0xdfff) {
      throw invalidMcpToolCall("contains an invalid Unicode string", {
        field: `mcpToolCall.${field}`,
      });
    } else if (codeUnit <= 0x7f) {
      decodedBytes += 1;
    } else if (codeUnit <= 0x7ff) {
      decodedBytes += 2;
    } else {
      decodedBytes += 3;
    }
    if (decodedBytes > MAX_MCP_TOOL_CALL_COMPONENT_BYTES) {
      throw invalidMcpToolCall(
        `exceeds ${MAX_MCP_TOOL_CALL_COMPONENT_BYTES} decoded UTF-8 bytes`,
        { field: `mcpToolCall.${field}` },
      );
    }
  }
  if (decodedBytes === 0) {
    throw invalidMcpToolCall("must be nonempty", { field: `mcpToolCall.${field}` });
  }
  return value;
}

function invalidMcpToolCall(message, details = {}) {
  return new CtxParseError(`agent-history-v1 MCP tool call ${message}`, {
    details: { field: "mcpToolCall", ...details },
  });
}

function normalizeStatus(raw) {
  const current = camelizeKeys(raw ?? {});
  const status = {};
  for (const key of [
    "initialized",
    "readOnly",
    "dataRoot",
    "indexedItems",
    "indexedSessions",
    "indexedEvents",
    "indexedSources",
    "historyEpoch",
    "lexical",
    "refresh",
    "semantic",
    "daemon",
  ]) {
    if (current[key] !== undefined) {
      if (key.startsWith("indexed")) {
        requireExactStatusCounter(key, current[key]);
      }
      status[key] = current[key];
    }
  }
  status.initialized ??= typeof current.lexical?.generationId === "string";
  status.localOnly = true;
  return status;
}

function requireExactStatusCounter(key, value) {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new CtxParseError(
      `ctx status counter ${key} is outside the exact JSON integer domain`,
      {
        details: { field: key, maximum: Number.MAX_SAFE_INTEGER },
      },
    );
  }
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
      camelKey === "configPath" ||
      camelKey === "itemType" ||
      camelKey === "payloadType" ||
      camelKey === "recordType"
    ) {
      continue;
    }
    out[camelKey] = camelizeKeys(item);
  }
  return out;
}

function snakeToCamel(value) {
  const parts = value.split("_");
  if (parts.length === 1) {
    return value;
  }
  return parts[0] + parts.slice(1).map((part) => (
    part.length === 0 ? "" : part[0].toUpperCase() + part.slice(1)
  )).join("");
}

function bridgeSearchPagination(search) {
  if (!search || typeof search !== "object" || Array.isArray(search) || "pagination" in search) {
    return;
  }
  const resultWindow = search.resultWindow;
  if (!resultWindow || typeof resultWindow !== "object" || Array.isArray(resultWindow)) {
    return;
  }
  const pagination = {};
  if ("limit" in resultWindow) {
    pagination.limit = resultWindow.limit;
  }
  if ("moreAvailable" in resultWindow) {
    pagination.hasMore = resultWindow.moreAvailable;
  }
  search.pagination = pagination;
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
  appendRepeated(args, "--term", options.terms ?? options.term);
  appendOptional(args, "--limit", options.limit);
  appendOptional(args, "--provider", options.provider);
  appendOptional(args, "--workspace", options.workspace);
  appendOptional(args, "--since", options.since);
  appendFlag(args, "--primary-only", options.primaryOnly);
  appendFlag(args, "--include-subagents", options.includeSubagents);
  appendOptional(args, "--content-scope", options.contentScope);
  appendOptional(args, "--event-type", options.eventType);
  appendOptional(args, "--file", options.file);
  appendOptional(args, "--session", options.session);
  appendFlag(args, "--events", options.events);
  appendOptional(args, "--backend", options.backend);
  appendOptional(args, "--semantic-weight", options.semanticWeight);
  appendOptional(args, "--refresh", options.refresh);
  appendFlag(args, "--include-current-session", options.includeCurrentSession);
}

function validateSearchOptions(options) {
  if (
    options.contentScope !== undefined &&
    options.contentScope !== null &&
    !["all", "transcript", "calls", "outputs"].includes(options.contentScope)
  ) {
    throw new CtxValidationError(
      "search contentScope must be one of all, transcript, calls, outputs",
      { details: { contentScope: options.contentScope } },
    );
  }
  if (
    options.contentScope !== undefined &&
    options.contentScope !== null &&
    options.eventType !== undefined &&
    options.eventType !== null
  ) {
    throw new CtxValidationError("search contentScope and eventType are mutually exclusive", {
      details: {
        contentScope: options.contentScope,
        eventType: options.eventType,
      },
    });
  }
  if (hasSearchText(options.query) || hasSearchText(options.file) || hasSearchTerm(options)) {
    return;
  }
  throw new CtxValidationError("search requires a query, term, or file option", {
    details: { options },
  });
}

function hasSearchTerm(options) {
  const value = options.terms ?? options.term;
  if (Array.isArray(value)) {
    return value.some(hasSearchText);
  }
  return hasSearchText(value);
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

function appendRepeated(args, flag, value) {
  const values = Array.isArray(value) ? value : value ? [value] : [];
  for (const item of values) {
    args.push(flag, item);
  }
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
    stdout: decodeUtf8Output(result.stdout, "stdout"),
    stderr: decodeUtf8Output(result.stderr, "stderr"),
  };
}

function decodeUtf8Output(value, stream) {
  if (value === undefined || value === null) return "";
  if (typeof value === "string") return value;
  if (value instanceof ArrayBuffer || ArrayBuffer.isView(value)) {
    try {
      return new TextDecoder("utf-8", { fatal: true }).decode(value);
    } catch (cause) {
      throw new CtxParseError(`ctx returned invalid UTF-8 on ${stream}`, {
        details: { stream },
        cause,
      });
    }
  }
  return String(value);
}

function parseCtxVersion(raw) {
  const match = raw.match(/^ctx\s+(.+)$/);
  return match ? match[1] : raw || undefined;
}
