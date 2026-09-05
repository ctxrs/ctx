import { CtxValidationError } from "./errors.js";

export function bridgeSearchPagination(search) {
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

export function appendImportArgs(args, options) {
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

export function appendSearchArgs(args, options) {
  appendRepeated(args, "--term", options.terms ?? options.term);
  appendOptional(args, "--limit", options.limit);
  appendOptional(args, "--provider", options.provider);
  appendOptional(args, "--workspace", options.workspace);
  appendOptional(args, "--since", options.since);
  appendFlag(args, "--primary-only", options.primaryOnly);
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

export function validateSearchOptions(options) {
  if (Object.prototype.hasOwnProperty.call(options, "includeSubagents")) {
    throw new CtxValidationError("search includeSubagents was removed; omit it", {
      details: { includeSubagents: options.includeSubagents },
    });
  }
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

export function appendSessionLookupArgs(args, options) {
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
    appendOptional(args, flag, item);
  }
}

function appendOptional(args, flag, value) {
  if (value !== undefined && value !== null && value !== false) {
    args.push(...(typeof value === "string" && value.startsWith("-") ? [`${flag}=${value}`] : [flag, value]));
  }
}

export function appendOptionalNumber(args, flag, value) {
  if (value !== undefined && value !== null) {
    args.push(...(String(value).startsWith("-") ? [`${flag}=${value}`] : [flag, String(value)]));
  }
}

function appendFlag(args, flag, value) {
  if (value) {
    args.push(flag);
  }
}

export function requireId(label, id) {
  if (!id || typeof id !== "string") {
    throw new CtxValidationError(`${label} is required`, {
      details: { value: id },
    });
  }
}
