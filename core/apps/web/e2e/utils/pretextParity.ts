import type { Page } from "@playwright/test";
import { seedDummyWorkspace } from "./seedDummyWorkspace";

export type MarkdownSample = {
  name: string;
  markdown: string;
};

export type RowParityMeasurement = {
  planned: number;
  actual: number;
  delta: number;
};

export type MessageParityParams = {
  content: string;
  expanded: boolean;
  attachments?: Array<{ kind: "image"; mime_type: string; data_base64: string; name?: string }>;
  viewportWidth?: number;
};

export type AssistantParityParams = {
  content: string;
  isComplete?: boolean;
  viewportWidth?: number;
};

export type AssistantStreamingParityParams = {
  fragments: readonly string[];
  viewportWidth?: number;
};

export type AssistantStreamingParityStep = {
  content: string;
  partial: RowParityMeasurement;
  complete: RowParityMeasurement;
  actualDelta: number;
  plannedDelta: number;
  structureEquivalent: boolean;
};

export type AssistantStreamingParityMeasurement = {
  steps: AssistantStreamingParityStep[];
};

export type TurnHeaderParityParams = {
  content: string;
  viewportWidth?: number;
};

type E2EWindow = Window & {
  __ctxE2E?: {
    measureMarkdownParity?: (samples: readonly MarkdownSample[], width: number) => Promise<Array<{
      name: string;
      planned: number;
      actual: number;
      delta: number;
    }>>;
    measureMessageParity?: (params: MessageParityParams) => Promise<RowParityMeasurement>;
    measureAssistantParity?: (params: AssistantParityParams) => Promise<RowParityMeasurement>;
    measureAssistantStreamingParity?: (params: AssistantStreamingParityParams) => Promise<AssistantStreamingParityMeasurement>;
    measureTurnHeaderParity?: (params: TurnHeaderParityParams) => Promise<RowParityMeasurement>;
  };
};

export async function openWorkbenchShell(page: Page): Promise<void> {
  const seed = await seedDummyWorkspace(page.request, {
    tasks: 0,
    sessionsPerTask: 0,
    turnsPerSession: 0,
  });
  await page.goto(`/workspaces/${seed.workspaceId}?ctxE2E=1`, { waitUntil: "domcontentloaded" });
  await page.waitForFunction(() => {
    const api = (window as E2EWindow).__ctxE2E;
    return (
      typeof api?.measureMarkdownParity === "function" &&
      typeof api?.measureMessageParity === "function" &&
      typeof api?.measureAssistantParity === "function" &&
      typeof api?.measureAssistantStreamingParity === "function" &&
      typeof api?.measureTurnHeaderParity === "function"
    );
  });
}

export async function measureMarkdownParity(
  page: Page,
  samples: readonly MarkdownSample[],
  width: number,
) {
  return page.evaluate(async ({ samples, width: nextWidth }) => {
    const api = (window as E2EWindow).__ctxE2E?.measureMarkdownParity;
    if (typeof api !== "function") {
      throw new Error("ctxE2E.measureMarkdownParity is unavailable");
    }
    return api(samples, nextWidth);
  }, { samples, width });
}

export async function measureMessageParity(
  page: Page,
  params: MessageParityParams,
): Promise<RowParityMeasurement> {
  return page.evaluate(async (nextParams) => {
    const api = (window as E2EWindow).__ctxE2E?.measureMessageParity;
    if (typeof api !== "function") {
      throw new Error("ctxE2E.measureMessageParity is unavailable");
    }
    return api(nextParams);
  }, params);
}

export async function measureAssistantParity(
  page: Page,
  params: AssistantParityParams,
): Promise<RowParityMeasurement> {
  return page.evaluate(async (nextParams) => {
    const api = (window as E2EWindow).__ctxE2E?.measureAssistantParity;
    if (typeof api !== "function") {
      throw new Error("ctxE2E.measureAssistantParity is unavailable");
    }
    return api(nextParams);
  }, params);
}

export async function measureAssistantStreamingParity(
  page: Page,
  params: AssistantStreamingParityParams,
): Promise<AssistantStreamingParityMeasurement> {
  return page.evaluate(async (nextParams) => {
    const api = (window as E2EWindow).__ctxE2E?.measureAssistantStreamingParity;
    if (typeof api !== "function") {
      throw new Error("ctxE2E.measureAssistantStreamingParity is unavailable");
    }
    return api(nextParams);
  }, params);
}

export async function measureTurnHeaderParity(
  page: Page,
  params: TurnHeaderParityParams,
): Promise<RowParityMeasurement> {
  return page.evaluate(async (nextParams) => {
    const api = (window as E2EWindow).__ctxE2E?.measureTurnHeaderParity;
    if (typeof api !== "function") {
      throw new Error("ctxE2E.measureTurnHeaderParity is unavailable");
    }
    return api(nextParams);
  }, params);
}
