import React from "react";
import ReactDOMClient from "react-dom/client";
import { flushSync } from "react-dom";
import { MemoMarkdown } from "../sessionView";
import type { SessionMarkdownInlineCodeDebugPayload } from "../sessionThread/sessionMarkdownInlineMeasurementDebug";
import {
  clearSessionTranscriptMeasurementAuthorities,
  measureSessionMarkdownDocumentWithAuthorities,
} from "../sessionThread/sessionTranscriptMeasurementAuthorities";
import { SESSION_THREAD_LAYOUT_STYLE } from "../sessionThread/sessionThreadLayoutTokens";

export type WorkbenchMarkdownParitySample = {
  name: string;
  markdown: string;
};

export type WorkbenchMarkdownParityMeasurement = {
  name: string;
  planned: number;
  actual: number;
  delta: number;
};

export type WorkbenchMarkdownPlannerDebug = SessionMarkdownInlineCodeDebugPayload;

type WorkbenchPlainTextDebug = {
  lineCount: number;
  lines: string[];
  lineWidths: number[];
  text: string;
  width: number;
};

export type WorkbenchMarkdownParityDebugMeasurement = WorkbenchMarkdownParityMeasurement & {
  debug: WorkbenchMarkdownPlannerDebug | null;
  plainTextDebug: WorkbenchPlainTextDebug | null;
  actualTextRects: Array<{
    text: string;
    rects: Array<{ x: number; y: number; width: number; height: number }>;
  }>;
};

type WorkbenchMarkdownDebugWindow = Window & {
  __ctxForceInlineCodeDebug?: boolean;
  __ctxInlineCodeDebugTarget?: string;
  __ctxInlineCodeDebugWidth?: number;
  __ctxInlineCodeDebug?: WorkbenchMarkdownPlannerDebug;
  __ctxForcePlainTextDebug?: boolean;
  __ctxPlainTextDebugTarget?: string;
  __ctxPlainTextDebugWidth?: number;
  __ctxPlainTextDebug?: WorkbenchPlainTextDebug;
};

let markdownScrollProbeRoot: ReactDOMClient.Root | null = null;
let markdownScrollProbeHost: HTMLElement | null = null;
let markdownScrollProbeContainer: HTMLElement | null = null;

function collectTextNodeRects(root: Element | null) {
  if (!root) return [];
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const nodes: Array<{
    text: string;
    rects: Array<{ x: number; y: number; width: number; height: number }>;
  }> = [];
  let current = walker.nextNode();
  while (current) {
    if (current.nodeType === Node.TEXT_NODE) {
      const text = current.textContent ?? "";
      if (text.trim().length > 0) {
        const range = document.createRange();
        range.selectNodeContents(current);
        const rects = Array.from(range.getClientRects()).map((rect) => ({
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height,
        }));
        nodes.push({ text, rects });
      }
    }
    current = walker.nextNode();
  }
  return nodes;
}

function applyMarkdownLayoutStyle(host: HTMLElement, width: number) {
  host.className = "wb-assistant-body";
  host.style.width = `${width}px`;
  Object.entries(SESSION_THREAD_LAYOUT_STYLE).forEach(([key, value]) => {
    host.style.setProperty(key, String(value));
  });
}

export async function measureWorkbenchMarkdownParity(
  samples: readonly WorkbenchMarkdownParitySample[],
  width: number,
): Promise<WorkbenchMarkdownParityMeasurement[]> {
  const out: WorkbenchMarkdownParityMeasurement[] = [];
  for (const sample of samples) {
    const planned = measureSessionMarkdownDocumentWithAuthorities(sample.markdown, width);
    const host = document.createElement("div");
    host.style.position = "fixed";
    host.style.left = "-10000px";
    host.style.top = "0";
    applyMarkdownLayoutStyle(host, width);
    document.body.appendChild(host);
    const root = ReactDOMClient.createRoot(host);
    flushSync(() => {
      root.render(React.createElement(MemoMarkdown, { content: sample.markdown }));
    });
    const actual = host.querySelector(".wb-markdown-root")?.getBoundingClientRect().height ?? 0;
    out.push({
      name: sample.name,
      planned,
      actual,
      delta: planned - actual,
    });
    root.unmount();
    host.remove();
  }
  return out;
}

export async function measureWorkbenchMarkdownParityDebug(
  markdown: string,
  width: number,
  target = "*",
): Promise<WorkbenchMarkdownParityDebugMeasurement> {
  clearSessionTranscriptMeasurementAuthorities();
  const debugWindow = window as WorkbenchMarkdownDebugWindow;
  debugWindow.__ctxForceInlineCodeDebug = true;
  debugWindow.__ctxInlineCodeDebugTarget = target;
  debugWindow.__ctxInlineCodeDebugWidth = width;
  debugWindow.__ctxInlineCodeDebug = undefined;
  debugWindow.__ctxForcePlainTextDebug = true;
  debugWindow.__ctxPlainTextDebugTarget = "*";
  debugWindow.__ctxPlainTextDebugWidth = width;
  debugWindow.__ctxPlainTextDebug = undefined;

  try {
    const planned = measureSessionMarkdownDocumentWithAuthorities(markdown, width);
    const host = document.createElement("div");
    host.style.position = "fixed";
    host.style.left = "-10000px";
    host.style.top = "0";
    applyMarkdownLayoutStyle(host, width);
    document.body.appendChild(host);
    const root = ReactDOMClient.createRoot(host);
    flushSync(() => {
      root.render(React.createElement(MemoMarkdown, { content: markdown }));
    });
    const markdownRoot = host.querySelector(".wb-markdown-root");
    const actual = markdownRoot?.getBoundingClientRect().height ?? 0;
    const actualTextRects = collectTextNodeRects(markdownRoot);
    root.unmount();
    host.remove();
    return {
      name: "debug",
      planned,
      actual,
      delta: planned - actual,
      debug: debugWindow.__ctxInlineCodeDebug ?? null,
      plainTextDebug: debugWindow.__ctxPlainTextDebug ?? null,
      actualTextRects,
    };
  } finally {
    debugWindow.__ctxForceInlineCodeDebug = false;
    debugWindow.__ctxInlineCodeDebugTarget = undefined;
    debugWindow.__ctxInlineCodeDebugWidth = undefined;
    debugWindow.__ctxInlineCodeDebug = undefined;
    debugWindow.__ctxForcePlainTextDebug = false;
    debugWindow.__ctxPlainTextDebugTarget = undefined;
    debugWindow.__ctxPlainTextDebugWidth = undefined;
    debugWindow.__ctxPlainTextDebug = undefined;
    clearSessionTranscriptMeasurementAuthorities();
  }
}

export async function measureWorkbenchMarkdownSelectionText(markdown: string, width: number): Promise<string> {
  const probe = document.createElement("div");
  probe.style.position = "fixed";
  probe.style.left = "24px";
  probe.style.top = "24px";
  probe.style.width = `${Math.max(1, width)}px`;
  probe.style.maxWidth = "calc(100vw - 48px)";
  probe.style.padding = "16px";
  probe.style.zIndex = "9999";
  probe.style.background = "var(--bg)";
  probe.style.border = "1px solid var(--border)";
  probe.style.pointerEvents = "none";
  probe.style.boxSizing = "border-box";
  document.body.appendChild(probe);

  const host = document.createElement("div");
  applyMarkdownLayoutStyle(host, width);
  probe.appendChild(host);
  const root = ReactDOMClient.createRoot(host);
  try {
    flushSync(() => {
      root.render(React.createElement(MemoMarkdown, { content: markdown }));
    });

    const markdownRoot = host.querySelector(".wb-markdown-root");
    if (!markdownRoot) {
      return "";
    }

    const selection = window.getSelection();
    const range = document.createRange();
    range.selectNodeContents(markdownRoot);
    selection?.removeAllRanges();
    selection?.addRange(range);
    await new Promise((resolve) => window.requestAnimationFrame(() => resolve(undefined)));
    const text = selection?.toString() ?? "";
    selection?.removeAllRanges();
    return text;
  } finally {
    root.unmount();
    probe.remove();
  }
}

export async function installWorkbenchMarkdownScrollProbe(markdown: string, width = 788): Promise<boolean> {
  removeWorkbenchMarkdownScrollProbe();

  const probe = document.createElement("div");
  probe.id = "markdown-scroll-probe";
  probe.style.position = "fixed";
  probe.style.right = "24px";
  probe.style.top = "24px";
  probe.style.width = "920px";
  probe.style.height = "560px";
  probe.style.zIndex = "9999";
  probe.style.background = "var(--bg)";
  probe.style.border = "1px solid var(--border)";
  document.body.appendChild(probe);

  const scroller = document.createElement("div");
  scroller.className = "wb-thread-scroller";
  scroller.setAttribute("data-pretext-virtualizer-list", "1");
  scroller.style.height = "100%";
  scroller.style.overflow = "auto";
  scroller.style.padding = "24px";
  probe.appendChild(scroller);

  const topFiller = document.createElement("div");
  topFiller.style.height = "420px";
  scroller.appendChild(topFiller);

  const host = document.createElement("div");
  applyMarkdownLayoutStyle(host, width);
  scroller.appendChild(host);

  const bottomFiller = document.createElement("div");
  bottomFiller.style.height = "480px";
  scroller.appendChild(bottomFiller);

  const root = ReactDOMClient.createRoot(host);
  flushSync(() => {
    root.render(React.createElement(MemoMarkdown, { content: markdown }));
  });
  scroller.scrollTop = 420;
  scroller.dispatchEvent(new Event("scroll", { bubbles: true }));

  markdownScrollProbeRoot = root;
  markdownScrollProbeHost = host;
  markdownScrollProbeContainer = probe;
  return true;
}

export function removeWorkbenchMarkdownScrollProbe(): void {
  markdownScrollProbeRoot?.unmount();
  markdownScrollProbeRoot = null;
  markdownScrollProbeHost?.remove();
  markdownScrollProbeHost = null;
  markdownScrollProbeContainer?.remove();
  markdownScrollProbeContainer = null;
}
