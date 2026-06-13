import fs from "node:fs/promises";
import { test, expect } from "./fixtures";
import {
  E2E_DIAGNOSTIC_BUNDLE_MAX_ARTIFACT_BYTES,
  E2E_DIAGNOSTIC_BUNDLE_SCHEMA_VERSION,
  buildE2EDiagnosticBundle,
  type E2EDiagnosticBundle,
  validateE2EDiagnosticBundle,
  writeE2EDiagnosticBundleForFailure,
} from "./utils/diagnostics";

const validBundle = (): E2EDiagnosticBundle => ({
  schema_version: E2E_DIAGNOSTIC_BUNDLE_SCHEMA_VERSION,
  commit: "ebff39ba6fcb3bb6e36355080630c4bb805ae6da",
  version: "0.65.131",
  test_name: "release updater web e2e",
  failure_phase: "webdriver_session",
  launch_state: "invoked",
  daemon_session_head: { session_id: "session-1", state_rev: 12, last_event_seq: 44 },
  workspace_active_head: { workspace_id: "workspace-1", snapshot_rev: 8 },
  visible_thread_state: { active_session_id: "session-1", rendered_turns: 2 },
  stream_reset_gap_counters: { session_gap: 0, replay_reset: 0 },
  subscription_plan: { active_primary_intent: "head", foreground_intent: "replay" },
  foreground_freshness_diagnostics: { state: "fresh", max_lag_ms: 0 },
  interrupt_telemetry_counters: { interrupt_requested: 0, turn_interrupted: 0 },
  browser_console_tail: ["error: WebDriver session failed before first page event"],
  screenshot_paths: ["screenshots/failure.png"],
  trace_paths: ["traces/release-updater.zip"],
  artifact_size_bytes: 2048,
});

test("web e2e diagnostic bundle schema accepts the deterministic release shape", () => {
  const result = validateE2EDiagnosticBundle(validBundle());
  expect(result.ok).toBe(true);
});

test("web e2e diagnostic failure writer emits an attached schema-valid bundle", async ({}, testInfo) => {
  await fs.mkdir(testInfo.outputPath("screenshots"), { recursive: true });
  await fs.mkdir(testInfo.outputPath("traces"), { recursive: true });
  await fs.writeFile(testInfo.outputPath("screenshots/failure.png"), "fake-screenshot-bytes", "utf8");
  await fs.writeFile(testInfo.outputPath("traces/release-updater.zip"), "fake-trace-bytes", "utf8");
  const written = await writeE2EDiagnosticBundleForFailure({
    bundle: {
      ...validBundle(),
      artifact_size_bytes: 0,
      test_name: testInfo.title,
    },
    testInfo,
  });
  const raw = JSON.parse(await fs.readFile(written.path, "utf8"));
  const result = validateE2EDiagnosticBundle(raw);
  expect(result.ok).toBe(true);
  expect(written.bundle.test_name).toBe(testInfo.title);
  expect(written.bundle.artifact_size_bytes).toBeGreaterThan(30);
});

test("web e2e diagnostic bundle schema rejects missing keys and invalid launch state", () => {
  const bundle = validBundle();
  const input: Record<string, unknown> = {
    ...bundle,
    launch_state: "started",
  };
  delete input.daemon_session_head;

  const result = validateE2EDiagnosticBundle(input);
  expect(result.ok).toBe(false);
  if (!result.ok) {
    expect(result.errors).toContain("missing required key 'daemon_session_head'");
    expect(result.errors.some((error) => error.includes("launch_state must be one of"))).toBe(true);
  }
});

test("web e2e diagnostic bundle schema rejects empty required evidence fields", () => {
  const result = validateE2EDiagnosticBundle({
    ...validBundle(),
    daemon_session_head: {},
    screenshot_paths: [],
    stream_reset_gap_counters: {},
  });

  expect(result.ok).toBe(false);
  if (!result.ok) {
    expect(result.errors).toContain("daemon_session_head must not be empty");
    expect(result.errors).toContain("screenshot_paths must not be empty");
    expect(result.errors).toContain("stream_reset_gap_counters must not be empty");
  }
});

test("web e2e diagnostic bundle builder fails closed for release-critical placeholder evidence", () => {
  expect(() => buildE2EDiagnosticBundle({
    failure_phase: "test_failure",
    launch_state: "invoked",
    release_critical: true,
  })).toThrow(/daemon_session_head must not be empty/);
});

test("web e2e diagnostic writer fails release-critical bundles with missing artifacts", async ({}, testInfo) => {
  await expect(writeE2EDiagnosticBundleForFailure({
    bundle: {
      ...validBundle(),
      release_critical: true,
      screenshot_paths: ["screenshots/missing.png"],
      trace_paths: ["traces/missing.zip"],
    },
    testInfo,
  })).rejects.toThrow(/release-critical diagnostic artifact is missing: screenshots\/missing\.png/);
});

test("web e2e diagnostic bundle schema rejects unsafe paths and oversized artifacts", () => {
  const result = validateE2EDiagnosticBundle({
    ...validBundle(),
    artifact_size_bytes: E2E_DIAGNOSTIC_BUNDLE_MAX_ARTIFACT_BYTES + 1,
    screenshot_paths: ["/tmp/failure.png"],
    trace_paths: ["../trace.zip"],
  });

  expect(result.ok).toBe(false);
  if (!result.ok) {
    expect(result.errors).toContain(`artifact_size_bytes exceeds ${E2E_DIAGNOSTIC_BUNDLE_MAX_ARTIFACT_BYTES}`);
    expect(result.errors).toContain("screenshot_paths[0] must be a safe relative artifact path");
    expect(result.errors).toContain("trace_paths[0] must be a safe relative artifact path");
  }
});
