import fs from "node:fs/promises";
import path from "node:path";
import { expect, type Page, type TestInfo } from "playwright/test";

export const E2E_DIAGNOSTIC_BUNDLE_SCHEMA_VERSION = "ctx.web_e2e.diagnostic_bundle.v1";
export const E2E_DIAGNOSTIC_BUNDLE_MAX_JSON_BYTES = 512 * 1024;
export const E2E_DIAGNOSTIC_BUNDLE_MAX_ARTIFACT_BYTES = 64 * 1024 * 1024;

const MAX_STRING_LENGTH = 4096;
const MAX_STATE_FIELD_BYTES = 64 * 1024;
const MAX_OBJECT_KEYS = 80;
const MAX_ARRAY_ITEMS = 128;
const MAX_BROWSER_CONSOLE_TAIL = 200;
const MAX_BROWSER_CONSOLE_ENTRY_LENGTH = 2000;
const MAX_PATH_LENGTH = 512;
const MAX_DEPTH = 6;

export const E2E_DIAGNOSTIC_LAUNCH_STATES = [
  "invoked",
  "exited_early",
  "never_launched",
  "unknown",
] as const;

export const E2E_DIAGNOSTIC_BUNDLE_REQUIRED_KEYS = [
  "schema_version",
  "commit",
  "version",
  "test_name",
  "failure_phase",
  "launch_state",
  "daemon_session_head",
  "workspace_active_head",
  "visible_thread_state",
  "stream_reset_gap_counters",
  "subscription_plan",
  "foreground_freshness_diagnostics",
  "interrupt_telemetry_counters",
  "browser_console_tail",
  "screenshot_paths",
  "trace_paths",
  "artifact_size_bytes",
] as const;

const E2E_DIAGNOSTIC_BUNDLE_REQUIRED_KEY_SET = new Set<string>(E2E_DIAGNOSTIC_BUNDLE_REQUIRED_KEYS);
const E2E_DIAGNOSTIC_LAUNCH_STATE_SET = new Set<string>(E2E_DIAGNOSTIC_LAUNCH_STATES);

export type E2EDiagnosticLaunchState = (typeof E2E_DIAGNOSTIC_LAUNCH_STATES)[number];
export type E2EDiagnosticJsonObject = Record<string, unknown>;

export type E2EDiagnosticBundle = {
  schema_version: typeof E2E_DIAGNOSTIC_BUNDLE_SCHEMA_VERSION;
  commit: string;
  version: string;
  test_name: string;
  failure_phase: string;
  launch_state: E2EDiagnosticLaunchState;
  daemon_session_head: E2EDiagnosticJsonObject;
  workspace_active_head: E2EDiagnosticJsonObject;
  visible_thread_state: E2EDiagnosticJsonObject;
  stream_reset_gap_counters: Record<string, number>;
  subscription_plan: E2EDiagnosticJsonObject;
  foreground_freshness_diagnostics: E2EDiagnosticJsonObject;
  interrupt_telemetry_counters: Record<string, number>;
  browser_console_tail: string[];
  screenshot_paths: string[];
  trace_paths: string[];
  artifact_size_bytes: number;
};

export type E2EDiagnosticBundleBuildInput = Partial<Omit<E2EDiagnosticBundle, "schema_version">> & {
  failure_phase: string;
  launch_state: E2EDiagnosticLaunchState;
  release_critical?: boolean;
};

export type E2EDiagnosticBundleWriteResult = {
  bundle: E2EDiagnosticBundle;
  path: string;
};

export type E2EDiagnosticBundleValidationResult =
  | { ok: true; bundle: E2EDiagnosticBundle }
  | { ok: false; errors: string[] };

type DiagnosticBundleTestInfo = Pick<TestInfo, "attach" | "outputPath" | "title">;

type E2EDiagnosticEvent = {
  id: number;
  ts: number;
  source: string;
  code: string;
  severity: "info" | "warning" | "error";
  message: string;
  fatal?: boolean;
  context?: Record<string, unknown>;
};

type NoUnexpectedDiagnosticsOptions = {
  includeWarnings?: boolean;
  allowedCodes?: string[];
};

type E2EWindow = Window & {
  __ctxE2E?: {
    clearDiagnostics?: () => void;
    getDiagnostics?: () => E2EDiagnosticEvent[] | unknown;
    getOpenedWebSocketUrls?: () => string[] | unknown;
  };
};

type BrowserDiagnosticSnapshot = {
  body_text_length: number;
  connection: Record<string, unknown>;
  diagnostics_tail: unknown[];
  document_title: string;
  e2e_hooks_present: boolean;
  location_hash: string;
  page_url: string;
  pathname: string;
  search: string;
  visibility_state: string;
  websocket_urls: string[];
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function jsonSizeBytes(value: unknown): number {
  try {
    return new TextEncoder().encode(JSON.stringify(value)).length;
  } catch {
    return Number.POSITIVE_INFINITY;
  }
}

function boundedStringFromValue(value: unknown, maxLength = MAX_STRING_LENGTH): string {
  return String(value || "").trim().slice(0, maxLength);
}

function defaultCommit(): string {
  const candidate = boundedStringFromValue(process.env.RELEASE_SOURCE_COMMIT || process.env.GITHUB_SHA || "");
  return /^[a-f0-9]{40}$/iu.test(candidate)
    ? candidate
    : "0000000000000000000000000000000000000000";
}

function defaultVersion(): string {
  const candidate = boundedStringFromValue(
    process.env.CTX_RELEASE_EFFECTIVE_VERSION || process.env.npm_package_version || "",
  );
  return /^[A-Za-z0-9][A-Za-z0-9._+-]{0,79}$/u.test(candidate) ? candidate : "0.0.0-local";
}

function normalizeBrowserConsoleTail(entries: readonly string[] | undefined): string[] {
  return (Array.isArray(entries) ? entries : [])
    .slice(-MAX_BROWSER_CONSOLE_TAIL)
    .map((entry) => boundedStringFromValue(entry, MAX_BROWSER_CONSOLE_ENTRY_LENGTH));
}

function normalizeArtifactPaths(entries: readonly string[] | undefined): string[] {
  return (Array.isArray(entries) ? entries : []).map((entry) => boundedStringFromValue(entry, MAX_PATH_LENGTH));
}

async function existingArtifactSizeBytes(
  outputDir: string,
  artifactPaths: readonly string[],
  { requireExisting = false }: { requireExisting?: boolean } = {},
): Promise<number> {
  let sizeBytes = 0;
  for (const artifactPath of artifactPaths) {
    if (!artifactPath || artifactPath.includes("\0") || artifactPath.startsWith("/") || artifactPath.split(/[\\/]/).includes("..")) {
      continue;
    }
    try {
      const stat = await fs.stat(path.join(outputDir, artifactPath));
      if (stat.isFile()) {
        sizeBytes += stat.size;
      }
    } catch (error) {
      if (!isRecord(error) || error.code !== "ENOENT") {
        throw error;
      }
      if (requireExisting) {
        throw new Error(`release-critical diagnostic artifact is missing: ${artifactPath}`);
      }
    }
  }
  return sizeBytes;
}

function defaultDiagnosticObject(field: string): E2EDiagnosticJsonObject {
  return {
    captured_by: "shared_web_e2e_failure_writer",
    unavailable_reason: `${field}_not_collected_by_test`,
  };
}

function countEvents(
  events: readonly Record<string, unknown>[],
  predicate: (event: Record<string, unknown>) => boolean,
): number {
  return events.filter(predicate).length;
}

function eventCodeIncludes(event: Record<string, unknown>, pattern: RegExp): boolean {
  return pattern.test(String(event.code || "")) || pattern.test(String(event.message || ""));
}

function validateBoundedJson(value: unknown, path: string, errors: string[], depth = 0): void {
  if (depth > MAX_DEPTH) {
    errors.push(`${path} exceeds max depth ${MAX_DEPTH}`);
    return;
  }
  if (value == null) {
    return;
  }
  const valueType = typeof value;
  if (valueType === "string") {
    if ((value as string).length > MAX_STRING_LENGTH) {
      errors.push(`${path} string exceeds ${MAX_STRING_LENGTH} characters`);
    }
    return;
  }
  if (valueType === "number") {
    if (!Number.isFinite(value as number)) {
      errors.push(`${path} must be a finite number`);
    }
    return;
  }
  if (valueType === "boolean") {
    return;
  }
  if (Array.isArray(value)) {
    if (value.length > MAX_ARRAY_ITEMS) {
      errors.push(`${path} has ${value.length} items; max is ${MAX_ARRAY_ITEMS}`);
      return;
    }
    value.forEach((entry, index) => validateBoundedJson(entry, `${path}[${index}]`, errors, depth + 1));
    return;
  }
  if (!isRecord(value)) {
    errors.push(`${path} is not JSON-serializable`);
    return;
  }
  const entries = Object.entries(value);
  if (entries.length > MAX_OBJECT_KEYS) {
    errors.push(`${path} has ${entries.length} keys; max is ${MAX_OBJECT_KEYS}`);
    return;
  }
  for (const [key, entry] of entries) {
    if (!/^[A-Za-z0-9_.:-]{1,80}$/.test(key) || key === "__proto__" || key === "constructor") {
      errors.push(`${path} has unsafe key '${key}'`);
      continue;
    }
    validateBoundedJson(entry, `${path}.${key}`, errors, depth + 1);
  }
}

function validateStructuredObject(value: unknown, key: string, errors: string[], options: { allowEmpty?: boolean } = {}): void {
  if (!isRecord(value)) {
    errors.push(`${key} must be an object`);
    return;
  }
  if (options.allowEmpty === false && Object.keys(value).length === 0) {
    errors.push(`${key} must not be empty`);
    return;
  }
  const size = jsonSizeBytes(value);
  if (!Number.isFinite(size) || size > MAX_STATE_FIELD_BYTES) {
    errors.push(`${key} exceeds ${MAX_STATE_FIELD_BYTES} encoded bytes`);
    return;
  }
  validateBoundedJson(value, key, errors);
}

function validateCounterRecord(value: unknown, key: string, errors: string[], options: { allowEmpty?: boolean } = {}): void {
  if (!isRecord(value)) {
    errors.push(`${key} must be an object of non-negative integer counters`);
    return;
  }
  const entries = Object.entries(value);
  if (options.allowEmpty === false && entries.length === 0) {
    errors.push(`${key} must not be empty`);
    return;
  }
  if (entries.length > MAX_OBJECT_KEYS) {
    errors.push(`${key} has ${entries.length} counters; max is ${MAX_OBJECT_KEYS}`);
    return;
  }
  for (const [counterKey, counterValue] of entries) {
    if (!/^[A-Za-z0-9_.:-]{1,80}$/.test(counterKey)) {
      errors.push(`${key} has unsafe counter key '${counterKey}'`);
      continue;
    }
    if (!Number.isSafeInteger(counterValue) || (counterValue as number) < 0) {
      errors.push(`${key}.${counterKey} must be a non-negative safe integer`);
    }
  }
}

function validateBoundedString(value: unknown, key: string, errors: string[], pattern?: RegExp): void {
  if (typeof value !== "string" || value.length === 0 || value.length > MAX_STRING_LENGTH) {
    errors.push(`${key} must be a non-empty string <= ${MAX_STRING_LENGTH} characters`);
    return;
  }
  if (pattern && !pattern.test(value)) {
    errors.push(`${key} has invalid format`);
  }
}

function validatePathArray(value: unknown, key: string, errors: string[], options: { allowEmpty?: boolean } = {}): void {
  if (!Array.isArray(value)) {
    errors.push(`${key} must be an array`);
    return;
  }
  if (options.allowEmpty === false && value.length === 0) {
    errors.push(`${key} must not be empty`);
    return;
  }
  if (value.length > MAX_ARRAY_ITEMS) {
    errors.push(`${key} has ${value.length} items; max is ${MAX_ARRAY_ITEMS}`);
    return;
  }
  value.forEach((entry, index) => {
    if (typeof entry !== "string" || entry.length === 0 || entry.length > MAX_PATH_LENGTH) {
      errors.push(`${key}[${index}] must be a non-empty string <= ${MAX_PATH_LENGTH} characters`);
      return;
    }
    if (entry.includes("\0") || entry.startsWith("/") || /^[A-Za-z]:[\\/]/.test(entry) || entry.split(/[\\/]/).includes("..")) {
      errors.push(`${key}[${index}] must be a safe relative artifact path`);
    }
  });
}

function validateBrowserConsoleTail(value: unknown, errors: string[]): void {
  if (!Array.isArray(value)) {
    errors.push("browser_console_tail must be an array");
    return;
  }
  if (value.length > MAX_BROWSER_CONSOLE_TAIL) {
    errors.push(`browser_console_tail has ${value.length} entries; max is ${MAX_BROWSER_CONSOLE_TAIL}`);
    return;
  }
  value.forEach((entry, index) => {
    if (typeof entry !== "string" || entry.length > MAX_BROWSER_CONSOLE_ENTRY_LENGTH) {
      errors.push(`browser_console_tail[${index}] must be a string <= ${MAX_BROWSER_CONSOLE_ENTRY_LENGTH} characters`);
    }
  });
}

export function validateE2EDiagnosticBundle(input: unknown): E2EDiagnosticBundleValidationResult {
  const errors: string[] = [];
  if (!isRecord(input)) {
    return { ok: false, errors: ["diagnostic bundle must be an object"] };
  }
  for (const key of E2E_DIAGNOSTIC_BUNDLE_REQUIRED_KEYS) {
    if (!(key in input)) {
      errors.push(`missing required key '${key}'`);
    }
  }
  for (const key of Object.keys(input)) {
    if (!E2E_DIAGNOSTIC_BUNDLE_REQUIRED_KEY_SET.has(key)) {
      errors.push(`unknown key '${key}'`);
    }
  }
  if (input.schema_version !== E2E_DIAGNOSTIC_BUNDLE_SCHEMA_VERSION) {
    errors.push(`schema_version must be '${E2E_DIAGNOSTIC_BUNDLE_SCHEMA_VERSION}'`);
  }
  validateBoundedString(input.commit, "commit", errors, /^[a-f0-9]{40}$/i);
  validateBoundedString(input.version, "version", errors, /^[A-Za-z0-9][A-Za-z0-9._+-]{0,79}$/);
  validateBoundedString(input.test_name, "test_name", errors);
  validateBoundedString(input.failure_phase, "failure_phase", errors, /^[a-z][a-z0-9_.:-]{0,79}$/);
  if (typeof input.launch_state !== "string" || !E2E_DIAGNOSTIC_LAUNCH_STATE_SET.has(input.launch_state)) {
    errors.push(`launch_state must be one of ${E2E_DIAGNOSTIC_LAUNCH_STATES.join(", ")}`);
  }
  validateStructuredObject(input.daemon_session_head, "daemon_session_head", errors, { allowEmpty: false });
  validateStructuredObject(input.workspace_active_head, "workspace_active_head", errors, { allowEmpty: false });
  validateStructuredObject(input.visible_thread_state, "visible_thread_state", errors, { allowEmpty: false });
  validateCounterRecord(input.stream_reset_gap_counters, "stream_reset_gap_counters", errors, { allowEmpty: false });
  validateStructuredObject(input.subscription_plan, "subscription_plan", errors, { allowEmpty: false });
  validateStructuredObject(input.foreground_freshness_diagnostics, "foreground_freshness_diagnostics", errors, { allowEmpty: false });
  validateCounterRecord(input.interrupt_telemetry_counters, "interrupt_telemetry_counters", errors, { allowEmpty: false });
  validateBrowserConsoleTail(input.browser_console_tail, errors);
  validatePathArray(input.screenshot_paths, "screenshot_paths", errors, { allowEmpty: false });
  validatePathArray(input.trace_paths, "trace_paths", errors, { allowEmpty: false });
  if (!Number.isSafeInteger(input.artifact_size_bytes) || (input.artifact_size_bytes as number) < 0) {
    errors.push("artifact_size_bytes must be a non-negative safe integer");
  } else if ((input.artifact_size_bytes as number) > E2E_DIAGNOSTIC_BUNDLE_MAX_ARTIFACT_BYTES) {
    errors.push(`artifact_size_bytes exceeds ${E2E_DIAGNOSTIC_BUNDLE_MAX_ARTIFACT_BYTES}`);
  }
  const encodedBytes = jsonSizeBytes(input);
  if (!Number.isFinite(encodedBytes) || encodedBytes > E2E_DIAGNOSTIC_BUNDLE_MAX_JSON_BYTES) {
    errors.push(`diagnostic bundle JSON exceeds ${E2E_DIAGNOSTIC_BUNDLE_MAX_JSON_BYTES} encoded bytes`);
  }
  if (errors.length > 0) {
    return { ok: false, errors };
  }
  return { ok: true, bundle: input as E2EDiagnosticBundle };
}

export function expectValidE2EDiagnosticBundle(input: unknown): E2EDiagnosticBundle {
  const result = validateE2EDiagnosticBundle(input);
  expect(result.ok, result.ok ? "" : result.errors.join("\n")).toBe(true);
  return (result as { ok: true; bundle: E2EDiagnosticBundle }).bundle;
}

export function buildE2EDiagnosticBundle(input: E2EDiagnosticBundleBuildInput): E2EDiagnosticBundle {
  const releaseCritical = input.release_critical === true;
  const screenshotPaths = normalizeArtifactPaths(input.screenshot_paths);
  const tracePaths = normalizeArtifactPaths(input.trace_paths);
  const bundle: E2EDiagnosticBundle = {
    schema_version: E2E_DIAGNOSTIC_BUNDLE_SCHEMA_VERSION,
    commit: boundedStringFromValue(input.commit || defaultCommit()),
    version: boundedStringFromValue(input.version || defaultVersion()),
    test_name: boundedStringFromValue(input.test_name || "unknown web e2e"),
    failure_phase: boundedStringFromValue(input.failure_phase),
    launch_state: input.launch_state,
    daemon_session_head: input.daemon_session_head || (releaseCritical ? {} : defaultDiagnosticObject("daemon_session_head")),
    workspace_active_head: input.workspace_active_head || (releaseCritical ? {} : defaultDiagnosticObject("workspace_active_head")),
    visible_thread_state: input.visible_thread_state || (releaseCritical ? {} : defaultDiagnosticObject("visible_thread_state")),
    stream_reset_gap_counters: input.stream_reset_gap_counters || (releaseCritical ? {} : { unavailable: 1 }),
    subscription_plan: input.subscription_plan || (releaseCritical ? {} : defaultDiagnosticObject("subscription_plan")),
    foreground_freshness_diagnostics: input.foreground_freshness_diagnostics || (releaseCritical ? {} : defaultDiagnosticObject("foreground_freshness_diagnostics")),
    interrupt_telemetry_counters: input.interrupt_telemetry_counters || (releaseCritical ? {} : { unavailable: 1 }),
    browser_console_tail: normalizeBrowserConsoleTail(input.browser_console_tail),
    screenshot_paths: screenshotPaths.length > 0 ? screenshotPaths : (releaseCritical ? [] : ["playwright-managed/screenshot-only-on-failure"]),
    trace_paths: tracePaths.length > 0 ? tracePaths : (releaseCritical ? [] : ["playwright-managed/trace-retain-on-failure"]),
    artifact_size_bytes: Number.isSafeInteger(input.artifact_size_bytes) && input.artifact_size_bytes >= 0
      ? input.artifact_size_bytes
      : 0,
  };
  const result = validateE2EDiagnosticBundle(bundle);
  if (!result.ok) {
    throw new Error(`invalid web e2e diagnostic bundle: ${result.errors.join("; ")}`);
  }
  return bundle;
}

export async function collectPageFailureDiagnosticBundleInput({
  browserConsoleTail,
  failurePhase,
  launchState,
  page,
  releaseCritical = Boolean(process.env.CTX_RELEASE_UPDATER_WEB_E2E_DIAGNOSTIC_DIR),
  testInfo,
}: {
  browserConsoleTail: readonly string[];
  failurePhase: string;
  launchState: E2EDiagnosticLaunchState;
  page: Page;
  releaseCritical?: boolean;
  testInfo: DiagnosticBundleTestInfo;
}): Promise<E2EDiagnosticBundleBuildInput> {
  const screenshotPaths: string[] = [];
  const tracePaths: string[] = [];
  const collectionErrors: string[] = [];
  const outputDir = path.dirname(testInfo.outputPath("ctx-web-e2e-diagnostic-bundle.json"));
  await fs.mkdir(outputDir, { recursive: true });

  const screenshotPath = testInfo.outputPath("diagnostic-screenshot.png");
  try {
    await page.screenshot({ fullPage: true, path: screenshotPath });
    screenshotPaths.push(path.basename(screenshotPath));
  } catch (error) {
    collectionErrors.push(`screenshot:${error instanceof Error ? error.message : String(error)}`);
  }

  const snapshot = await page.evaluate((): BrowserDiagnosticSnapshot => {
    const w = window as E2EWindow;
    const connection: Record<string, unknown> = {};
    const rawConnection = window.sessionStorage.getItem("ctxDaemonConnectionV1");
    if (rawConnection) {
      try {
        const parsed = JSON.parse(rawConnection) as Record<string, unknown>;
        for (const [key, value] of Object.entries(parsed)) {
          if (key === "authToken") {
            connection.auth_token_present = String(value || "").length > 0;
          } else {
            connection[key] = value;
          }
        }
      } catch {
        connection.parse_error = true;
      }
    } else {
      connection.missing = true;
    }
    const diagnostics = w.__ctxE2E?.getDiagnostics?.();
    const websocketUrls = w.__ctxE2E?.getOpenedWebSocketUrls?.();
    return {
      body_text_length: document.body?.innerText?.length ?? 0,
      connection,
      diagnostics_tail: Array.isArray(diagnostics) ? diagnostics.slice(-50) : [],
      document_title: document.title,
      e2e_hooks_present: Boolean(w.__ctxE2E),
      location_hash: window.location.hash,
      page_url: window.location.href,
      pathname: window.location.pathname,
      search: window.location.search,
      visibility_state: document.visibilityState,
      websocket_urls: Array.isArray(websocketUrls) ? websocketUrls.map(String).slice(-50) : [],
    };
  }).catch((error): BrowserDiagnosticSnapshot => {
    collectionErrors.push(`snapshot:${error instanceof Error ? error.message : String(error)}`);
    return {
      body_text_length: 0,
      connection: { snapshot_failed: true },
      diagnostics_tail: [],
      document_title: "",
      e2e_hooks_present: false,
      location_hash: "",
      page_url: "",
      pathname: "",
      search: "",
      visibility_state: "unknown",
      websocket_urls: [],
    };
  });

  const diagnosticEvents = snapshot.diagnostics_tail.filter(isRecord);
  const foregroundEvents = diagnosticEvents.filter((event) => String(event.source || "") === "foreground_freshness");
  const tracePath = testInfo.outputPath("diagnostic-trace.json");
  await fs.writeFile(tracePath, `${JSON.stringify({ collection_errors: collectionErrors, snapshot }, null, 2)}\n`, "utf8");
  tracePaths.push(path.basename(tracePath));

  return {
    browser_console_tail: browserConsoleTail,
    daemon_session_head: {
      connection: snapshot.connection,
      page_url: boundedStringFromValue(snapshot.page_url),
      websocket_url_count: snapshot.websocket_urls.length,
    },
    failure_phase: failurePhase,
    foreground_freshness_diagnostics: {
      event_count: foregroundEvents.length,
      latest_event: foregroundEvents.at(-1) || null,
      warning_count: countEvents(foregroundEvents, (event) => String(event.severity || "") === "warning"),
      error_count: countEvents(foregroundEvents, (event) => String(event.severity || "") === "error"),
    },
    interrupt_telemetry_counters: {
      interrupt_events: countEvents(diagnosticEvents, (event) => eventCodeIncludes(event, /interrupt/iu)),
    },
    launch_state: launchState,
    release_critical: releaseCritical,
    screenshot_paths: screenshotPaths,
    stream_reset_gap_counters: {
      gap_events: countEvents(diagnosticEvents, (event) => eventCodeIncludes(event, /gap/iu)),
      reset_events: countEvents(diagnosticEvents, (event) => eventCodeIncludes(event, /reset/iu)),
    },
    subscription_plan: {
      e2e_hooks_present: snapshot.e2e_hooks_present,
      websocket_url_count: snapshot.websocket_urls.length,
      websocket_urls: snapshot.websocket_urls.map((url) => boundedStringFromValue(url, MAX_PATH_LENGTH)),
    },
    trace_paths: tracePaths,
    visible_thread_state: {
      body_text_length: snapshot.body_text_length,
      document_title: boundedStringFromValue(snapshot.document_title),
      visibility_state: boundedStringFromValue(snapshot.visibility_state),
    },
    workspace_active_head: {
      location_hash: boundedStringFromValue(snapshot.location_hash),
      pathname: boundedStringFromValue(snapshot.pathname),
      search: boundedStringFromValue(snapshot.search),
    },
  };
}

export async function writeE2EDiagnosticBundleForFailure({
  attachmentName = "ctx-web-e2e-diagnostic-bundle",
  bundle,
  testInfo,
}: {
  attachmentName?: string;
  bundle: E2EDiagnosticBundleBuildInput;
  testInfo: DiagnosticBundleTestInfo;
}): Promise<E2EDiagnosticBundleWriteResult> {
  const outputPath = testInfo.outputPath("ctx-web-e2e-diagnostic-bundle.json");
  const outputDir = path.dirname(outputPath);
  const initialBundle = buildE2EDiagnosticBundle({
    ...bundle,
    test_name: bundle.test_name || testInfo.title,
  });
  const releaseCritical = bundle.release_critical === true;
  const artifactFileSizeBytes = await existingArtifactSizeBytes(outputDir, [
    ...initialBundle.screenshot_paths,
    ...initialBundle.trace_paths,
  ], { requireExisting: releaseCritical });
  const sizedBundle = buildE2EDiagnosticBundle({
    ...initialBundle,
    artifact_size_bytes: Math.max(
      initialBundle.artifact_size_bytes,
      jsonSizeBytes(initialBundle) + artifactFileSizeBytes,
    ),
  });
  await fs.mkdir(outputDir, { recursive: true });
  await fs.writeFile(outputPath, `${JSON.stringify(sizedBundle, null, 2)}\n`, "utf8");
  const diagnosticDir = boundedStringFromValue(process.env.CTX_RELEASE_UPDATER_WEB_E2E_DIAGNOSTIC_DIR || "", 2048);
  if (diagnosticDir) {
    const diagnosticPath = path.join(
      diagnosticDir,
      `${boundedStringFromValue(testInfo.title, 120).replace(/[^A-Za-z0-9._-]+/gu, "-") || "web-e2e"}-ctx-web-e2e-diagnostic-bundle.json`,
    );
    await fs.mkdir(path.dirname(diagnosticPath), { recursive: true });
    await fs.copyFile(outputPath, diagnosticPath);
  }
  await testInfo.attach(attachmentName, {
    contentType: "application/json",
    path: outputPath,
  });
  return {
    bundle: sizedBundle,
    path: outputPath,
  };
}

export async function clearDiagnostics(page: Page): Promise<void> {
  await page.evaluate(() => {
    (window as E2EWindow).__ctxE2E?.clearDiagnostics?.();
  });
}

export async function getDiagnostics(page: Page): Promise<E2EDiagnosticEvent[]> {
  return page.evaluate(() => {
    const events = (window as E2EWindow).__ctxE2E?.getDiagnostics?.();
    return Array.isArray(events) ? events : [];
  });
}

export async function expectNoUnexpectedDiagnostics(
  page: Page,
  opts?: NoUnexpectedDiagnosticsOptions,
): Promise<void> {
  const events = await getDiagnostics(page);
  const allowed = new Set(opts?.allowedCodes ?? []);
  const severitySet = new Set<string>(opts?.includeWarnings ? ["error", "warning"] : ["error"]);
  const unexpected = events.filter((event) => severitySet.has(String(event.severity)) && !allowed.has(event.code));
  expect(
    unexpected,
    `Unexpected diagnostics:\n${JSON.stringify(unexpected, null, 2)}`,
  ).toEqual([]);
}
