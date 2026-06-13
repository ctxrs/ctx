import fs from "fs/promises";
import { test, expect } from "./fixtures";
import type { CDPSession } from "playwright/test";
import { seedDummyWorkspace, startStreamingMessages } from "./utils/seedDummyWorkspace";

const PERF_PROBE_ENABLED = process.env.CTX_PRETEXT_PERF_PROBE === "1";
const PERF_GUARDRAIL_ENABLED = process.env.CTX_PRETEXT_PERF_GUARDRAIL === "1";
const TASK_COUNT = Number(process.env.CTX_PRETEXT_PERF_TASKS ?? "5");
const TURNS_PER_SESSION = Number(process.env.CTX_PRETEXT_PERF_TURNS ?? "28");
const STREAM_DURATION_MS = Number(process.env.CTX_PRETEXT_PERF_STREAM_DURATION_MS ?? "7000");
const STREAM_INTERVAL_MS = Number(process.env.CTX_PRETEXT_PERF_STREAM_INTERVAL_MS ?? "140");
const OPEN_ONLY = process.env.CTX_PRETEXT_PERF_OPEN_ONLY === "1";
const DISABLE_STREAM = process.env.CTX_PRETEXT_PERF_DISABLE_STREAM === "1";
const SCROLLER_SELECTOR = ".wb-session-slot[aria-hidden=\"false\"] [data-pretext-virtualizer-list=\"1\"]";

const PERF_MODES = [
  { name: "warm-full", warmMode: "full" },
  { name: "warm-view", warmMode: "view" },
  { name: "warm-off", warmMode: "off" },
] as const;

type PerfBucketMap = Record<string, number>;

type LoadTestSnapshot = {
  long_tasks?: Array<{ duration_ms: number }>;
  memory_samples?: Array<{ used_js_heap_size?: number; total_js_heap_size?: number }>;
};

type PretextPerfSnapshot = {
  counters?: Record<string, number>;
  buckets?: Record<string, PerfBucketMap>;
};

test.use({ browserName: "chromium" });

function percentile(values: number[], p: number): number | null {
  if (values.length === 0) return null;
  const sorted = values.slice().sort((left, right) => left - right);
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * p) - 1));
  return Math.round(sorted[index]! * 10) / 10;
}

function topEntries(bucket: PerfBucketMap | undefined, limit = 10): Array<{ key: string; count: number }> {
  return Object.entries(bucket ?? {})
    .sort((left, right) => right[1] - left[1])
    .slice(0, limit)
    .map(([key, count]) => ({ key, count }));
}

function selectMetric(metrics: Array<{ name: string; value: number }>, name: string): number | null {
  const entry = metrics.find((metric) => metric.name === name);
  return typeof entry?.value === "number" ? entry.value : null;
}

async function collectCdpMetrics(session: CDPSession): Promise<Record<string, number | null>> {
  const metrics = await session.send("Performance.getMetrics");
  return {
    TaskDuration: selectMetric(metrics.metrics, "TaskDuration"),
    ScriptDuration: selectMetric(metrics.metrics, "ScriptDuration"),
    LayoutDuration: selectMetric(metrics.metrics, "LayoutDuration"),
    RecalcStyleDuration: selectMetric(metrics.metrics, "RecalcStyleDuration"),
    JSHeapUsedSize: selectMetric(metrics.metrics, "JSHeapUsedSize"),
    JSHeapTotalSize: selectMetric(metrics.metrics, "JSHeapTotalSize"),
  };
}

async function readPageDiagnostics(page: Parameters<typeof test>[0]["page"]): Promise<{
  loadTest: LoadTestSnapshot | null;
  pretextPerf: PretextPerfSnapshot | null;
}> {
  return page.evaluate(() => {
    const loadTest = (window as Window & {
      __ctxLoadTestTelemetry?: {
        getSnapshot: () => LoadTestSnapshot;
      };
    }).__ctxLoadTestTelemetry;
    const pretextPerf = (window as Window & {
      __ctxPretextPerfDiagnostics?: {
        getSnapshot: () => PretextPerfSnapshot;
      };
    }).__ctxPretextPerfDiagnostics;
    return {
      loadTest: loadTest?.getSnapshot() ?? null,
      pretextPerf: pretextPerf?.getSnapshot() ?? null,
    };
  });
}

async function resetPageDiagnostics(page: Parameters<typeof test>[0]["page"]): Promise<void> {
  await page.evaluate(() => {
    (window as Window & {
      __ctxLoadTestTelemetry?: { reset: () => void };
      __ctxPretextPerfDiagnostics?: { reset: () => void };
    }).__ctxLoadTestTelemetry?.reset();
    (window as Window & {
      __ctxPretextPerfDiagnostics?: { reset: () => void };
    }).__ctxPretextPerfDiagnostics?.reset();
  });
}

for (const mode of PERF_MODES) {
  test(`workbench: pretext perf diagnostics (${mode.name})`, async ({ page, request }, testInfo) => {
    test.skip(
      !PERF_PROBE_ENABLED && !PERF_GUARDRAIL_ENABLED,
      "Set CTX_PRETEXT_PERF_PROBE=1 or CTX_PRETEXT_PERF_GUARDRAIL=1 to run the pretext perf diagnostics.",
    );
    test.skip(PERF_GUARDRAIL_ENABLED && mode.name !== "warm-off", "Guardrail mode only runs the warm-off baseline.");
    test.setTimeout(240_000);

    const messagePrefix = [
      "# Perf Probe",
      "",
      "- markdown bullet",
      "- another bullet",
      "",
      "```ts",
      "const probe = true;",
      "```",
      "",
      "| metric | value |",
      "| --- | --- |",
      "| alpha | beta |",
      "",
      "Paragraph with inline `code`, a long wrapping sentence, and repeated content for measurement churn.",
    ].join("\n");

    const seed = await seedDummyWorkspace(request, {
      tasks: TASK_COUNT,
      sessionsPerTask: 1,
      turnsPerSession: TURNS_PER_SESSION,
      throttleMs: 1,
      includeToolSummaries: true,
      toolSummariesPerTurn: 4,
      messageBytes: 3200,
      messagePrefix,
    });

    const query = new URLSearchParams({
      loadtest: "1",
      perfdiag: "1",
      pretextWarmMode: mode.warmMode,
    });

    await page.setViewportSize({ width: 1440, height: 960 });
    const cdp = await page.context().newCDPSession(page);
    await cdp.send("Performance.enable");

    await page.goto(`/workspaces/${seed.workspaceId}?${query.toString()}`, { waitUntil: "domcontentloaded" });
    const rows = page.locator(".wb-task-row");
    const sessionView = page.locator(".wb-session-slot[aria-hidden=\"false\"]");
    await expect(rows).toHaveCount(TASK_COUNT, { timeout: 30_000 });
    await page.waitForFunction(() => {
      const win = window as Window & {
        __ctxLoadTestTelemetry?: unknown;
        __ctxPretextPerfDiagnostics?: unknown;
      };
      return Boolean(win.__ctxLoadTestTelemetry && win.__ctxPretextPerfDiagnostics);
    });

    const openTask = async (taskNumber: number) => {
      const row = rows.filter({ hasText: `fixture task ${taskNumber}` }).first();
      await row.click();
      await expect(sessionView).toContainText(`${taskNumber}.1.1`, { timeout: 30_000 });
    };

    await openTask(1);
    await page.waitForTimeout(800);
    const initialOpenDiagnostics = await readPageDiagnostics(page);
    const initialOpenCdpMetrics = await collectCdpMetrics(cdp);

    if (OPEN_ONLY) {
      const openOnlySummary = {
        mode: mode.name,
        workspaceId: seed.workspaceId,
        phase: "open-only",
        initialOpen: {
          longTaskCount: initialOpenDiagnostics.loadTest?.long_tasks?.length ?? 0,
          maxLongTaskMs: Math.max(
            0,
            ...(initialOpenDiagnostics.loadTest?.long_tasks?.map((entry) => entry.duration_ms) ?? [0]),
          ),
          peakUsedJsHeapSize: Math.max(
            0,
            ...(initialOpenDiagnostics.loadTest?.memory_samples?.map((entry) => entry.used_js_heap_size ?? 0) ?? [0]),
          ),
          counters: initialOpenDiagnostics.pretextPerf?.counters ?? {},
          topRelayoutReasons: topEntries(initialOpenDiagnostics.pretextPerf?.buckets?.pretext_full_relayout_reason),
          topRowLayoutItems: topEntries(initialOpenDiagnostics.pretextPerf?.buckets?.pretext_row_layout_item),
          topMarkdownDocuments: topEntries(initialOpenDiagnostics.pretextPerf?.buckets?.pretext_markdown_document_key),
          topWarmSessions: topEntries(initialOpenDiagnostics.pretextPerf?.buckets?.pretext_warm_runtime_session),
          cdpMetrics: initialOpenCdpMetrics,
        },
      };
      await testInfo.attach(`pretext-perf-${mode.name}.json`, {
        body: JSON.stringify(openOnlySummary, null, 2),
        contentType: "application/json",
      });
      await fs.writeFile(
        testInfo.outputPath(`pretext-perf-${mode.name}.json`),
        JSON.stringify(openOnlySummary, null, 2),
        "utf8",
      );
      expect(initialOpenDiagnostics.pretextPerf).not.toBeNull();
      if (PERF_GUARDRAIL_ENABLED) {
        const counters = initialOpenDiagnostics.pretextPerf?.counters ?? {};
        expect(counters.pretext_full_relayout_calls ?? 0).toBeLessThanOrEqual(8);
        expect(counters.pretext_visible_sync_items_calls ?? 0).toBeLessThanOrEqual(48);
        expect(counters.pretext_row_layout_calls ?? 0).toBeLessThanOrEqual(1100);
        expect(counters.pretext_markdown_document_calls ?? 0).toBeLessThanOrEqual(128);
        expect(openOnlySummary.initialOpen.longTaskCount).toBeLessThanOrEqual(4);
        expect(openOnlySummary.initialOpen.maxLongTaskMs).toBeLessThanOrEqual(250);
        expect(openOnlySummary.initialOpen.peakUsedJsHeapSize).toBeLessThanOrEqual(40_000_000);
        expect(openOnlySummary.initialOpen.cdpMetrics.TaskDuration ?? 0).toBeLessThanOrEqual(1.5);
      }
      return;
    }

    await resetPageDiagnostics(page);

    const sessionIds = Object.values(seed.sessionIdsByTask).flat();
    const streamer = DISABLE_STREAM
      ? null
      : startStreamingMessages(request, {
          sessionIds,
          intervalMs: STREAM_INTERVAL_MS,
          durationMs: STREAM_DURATION_MS,
          messagePrefix,
          messageBytes: 3200,
          includeToolSummaries: true,
          toolSummariesPerTurn: 4,
        });

    const orderedTasks = Array.from({ length: TASK_COUNT }, (_, index) => index + 1);
    const switchOrder = [
      ...orderedTasks.slice(1),
      orderedTasks[0]!,
      ...orderedTasks.slice().reverse(),
    ];
    const switchDurationsMs: number[] = [];
    let crashMessage: string | null = null;
    let failure: unknown = null;

    try {
      for (let index = 0; index < switchOrder.length; index += 1) {
        const taskNumber = switchOrder[index]!;
        const start = await page.evaluate(() => performance.now());
        await openTask(taskNumber);
        const end = await page.evaluate(() => performance.now());
        switchDurationsMs.push(end - start);

        if (index % 3 === 2) {
          const scroller = page.locator(SCROLLER_SELECTOR).first();
          await scroller.evaluate((element) => {
            const maxScrollTop = Math.max(0, element.scrollHeight - element.clientHeight);
            element.scrollTop = Math.max(0, maxScrollTop - 900);
            element.dispatchEvent(new Event("scroll"));
          });
          await page.waitForTimeout(120);
          await scroller.evaluate((element) => {
            element.scrollTop = Math.max(0, element.scrollHeight - element.clientHeight);
            element.dispatchEvent(new Event("scroll"));
          });
        }

        await page.waitForTimeout(180);
      }
    } catch (error) {
      failure = error;
      crashMessage = error instanceof Error ? error.message : String(error);
    } finally {
      await streamer?.stop().catch(() => undefined);
    }

    let diagnostics: Awaited<ReturnType<typeof readPageDiagnostics>> = {
      loadTest: null,
      pretextPerf: null,
    };
    let cdpMetrics: Record<string, number | null> | null = null;
    try {
      diagnostics = await readPageDiagnostics(page);
      cdpMetrics = await collectCdpMetrics(cdp);
    } catch (error) {
      crashMessage ??= error instanceof Error ? error.message : String(error);
    }

    const loadTest = diagnostics.loadTest;
    const pretextPerf = diagnostics.pretextPerf;

    const summary = {
      mode: mode.name,
      workspaceId: seed.workspaceId,
      phase: DISABLE_STREAM ? "switch-no-stream" : "switch-with-stream",
      crashMessage,
      initialOpen: {
        longTaskCount: initialOpenDiagnostics.loadTest?.long_tasks?.length ?? 0,
        maxLongTaskMs: Math.max(
          0,
          ...(initialOpenDiagnostics.loadTest?.long_tasks?.map((entry) => entry.duration_ms) ?? [0]),
        ),
        peakUsedJsHeapSize: Math.max(
          0,
          ...(initialOpenDiagnostics.loadTest?.memory_samples?.map((entry) => entry.used_js_heap_size ?? 0) ?? [0]),
        ),
        counters: initialOpenDiagnostics.pretextPerf?.counters ?? {},
        topRelayoutReasons: topEntries(initialOpenDiagnostics.pretextPerf?.buckets?.pretext_full_relayout_reason),
        topRowLayoutItems: topEntries(initialOpenDiagnostics.pretextPerf?.buckets?.pretext_row_layout_item),
        topMarkdownDocuments: topEntries(initialOpenDiagnostics.pretextPerf?.buckets?.pretext_markdown_document_key),
        topWarmSessions: topEntries(initialOpenDiagnostics.pretextPerf?.buckets?.pretext_warm_runtime_session),
        cdpMetrics: initialOpenCdpMetrics,
      },
      switchDurationsMs,
      switchP50Ms: percentile(switchDurationsMs, 0.5),
      switchP95Ms: percentile(switchDurationsMs, 0.95),
      longTaskCount: loadTest?.long_tasks?.length ?? 0,
      maxLongTaskMs: Math.max(0, ...(loadTest?.long_tasks?.map((entry) => entry.duration_ms) ?? [0])),
      peakUsedJsHeapSize:
        Math.max(0, ...(loadTest?.memory_samples?.map((entry) => entry.used_js_heap_size ?? 0) ?? [0])),
      peakTotalJsHeapSize:
        Math.max(0, ...(loadTest?.memory_samples?.map((entry) => entry.total_js_heap_size ?? 0) ?? [0])),
      counters: pretextPerf?.counters ?? {},
      topRelayoutReasons: topEntries(pretextPerf?.buckets?.pretext_full_relayout_reason),
      topRowLayoutItems: topEntries(pretextPerf?.buckets?.pretext_row_layout_item),
      topMarkdownDocuments: topEntries(pretextPerf?.buckets?.pretext_markdown_document_key),
      topWarmSessions: topEntries(pretextPerf?.buckets?.pretext_warm_runtime_session),
      cdpMetrics,
    };

    await testInfo.attach(`pretext-perf-${mode.name}.json`, {
      body: JSON.stringify(summary, null, 2),
      contentType: "application/json",
    });
    await fs.writeFile(
      testInfo.outputPath(`pretext-perf-${mode.name}.json`),
      JSON.stringify(summary, null, 2),
      "utf8",
    );

    expect(loadTest).not.toBeNull();
    expect(pretextPerf).not.toBeNull();
    expect((pretextPerf?.counters?.pretext_row_layout_calls ?? 0) > 0).toBe(true);
    if (failure) {
      throw failure;
    }
  });
}
