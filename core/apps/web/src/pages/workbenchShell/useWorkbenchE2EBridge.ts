import { useEffect } from "react";
import {
  measureWorkbenchDiffFile,
  measureWorkbenchHarnessOption,
  measureWorkbenchTargets,
  type WorkbenchE2EMeasuredTargetResult,
  type WorkbenchE2EMeasureTargetsResult,
} from "./workbenchE2EMeasurements";
import {
  installWorkbenchMarkdownScrollProbe,
  measureWorkbenchMarkdownParityDebug,
  measureWorkbenchMarkdownParity,
  measureWorkbenchMarkdownSelectionText,
  removeWorkbenchMarkdownScrollProbe,
  type WorkbenchMarkdownParityDebugMeasurement,
  type WorkbenchMarkdownParityMeasurement,
  type WorkbenchMarkdownParitySample,
} from "./workbenchE2EMarkdown";
import {
  measureWorkbenchAssistantParity,
  measureWorkbenchAssistantStreamingParity,
  measureWorkbenchMessageParity,
  measureWorkbenchTurnHeaderParity,
  type WorkbenchAssistantParityParams,
  type WorkbenchAssistantStreamingParityMeasurement,
  type WorkbenchAssistantStreamingParityParams,
  type WorkbenchMessageParityParams,
  type WorkbenchRowParityMeasurement,
  type WorkbenchTurnHeaderParityParams,
} from "./workbenchE2ETranscriptParity";
import type { WorktreeVcsSnapshot } from "@ctx/types";

const E2E_IMAGE_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+lmZYAAAAASUVORK5CYII=";
const E2E_DESKTOP_DRAG_DROP_TEST_EVENT = "ctx:desktop-drag-drop-test";

type WorkbenchE2EPasteImageOptions = {
  base64?: string;
  htmlSrc?: string;
  includeFile?: boolean;
  text?: string;
};

type WorkbenchE2EDispatchResult = {
  ok: boolean;
  error?: string;
};

type ClipboardItemLike = {
  kind: "file";
  getAsFile: () => File;
};

type ClipboardTransferLike = {
  files: File[];
  items: ClipboardItemLike[];
  types: string[];
  getData: (type: string) => string;
};

function decodeBase64Buffer(value: string): ArrayBuffer {
  const raw = atob(value);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i += 1) {
    bytes[i] = raw.charCodeAt(i);
  }
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
}

function makePasteEvent(transfer: DataTransfer | ClipboardTransferLike): Event {
  if (typeof ClipboardEvent === "function") {
    try {
      const event = new ClipboardEvent("paste", {
        bubbles: true,
        cancelable: true,
        clipboardData: transfer instanceof DataTransfer ? transfer : undefined,
      });
      if (event.clipboardData !== transfer) {
        Object.defineProperty(event, "clipboardData", {
          configurable: true,
          value: transfer,
        });
      }
      return event;
    } catch {
      // Some runtimes reject synthetic clipboard payloads via the constructor.
    }
  }

  const event = new Event("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", {
    configurable: true,
    value: transfer,
  });
  return event;
}

function makeClipboardTransfer(options?: WorkbenchE2EPasteImageOptions): DataTransfer | ClipboardTransferLike {
  if (typeof DataTransfer === "function") {
    const transfer = new DataTransfer();
    if (options?.includeFile ?? true) {
      const bytes = decodeBase64Buffer(options?.base64 || E2E_IMAGE_BASE64);
      const file = new File([bytes], "paste.png", { type: "image/png" });
      transfer.items.add(file);
    }
    if (options?.htmlSrc) {
      transfer.setData("text/html", `<img src="${options.htmlSrc}" alt="pasted image">`);
    }
    if (options?.text) {
      transfer.setData("text/plain", options.text);
    }
    return transfer;
  }

  const files: File[] = [];
  const items: ClipboardItemLike[] = [];
  const data = new Map<string, string>();
  const types: string[] = [];

  if (options?.includeFile ?? true) {
    const bytes = decodeBase64Buffer(options?.base64 || E2E_IMAGE_BASE64);
    const file = new File([bytes], "paste.png", { type: "image/png" });
    files.push(file);
    items.push({
      kind: "file",
      getAsFile: () => file,
    });
    types.push("Files");
  }
  if (options?.htmlSrc) {
    data.set("text/html", `<img src="${options.htmlSrc}" alt="pasted image">`);
    types.push("text/html");
  }
  if (options?.text) {
    data.set("text/plain", options.text);
    types.push("text/plain");
  }

  return {
    files,
    items,
    types,
    getData: (type: string) => data.get(type) ?? "",
  };
}

type WorkbenchE2EWindow = Window & {
  __ctxE2E?: {
    focusNewTask?: () => boolean;
    clearDraftHarness?: () => boolean;
    focusTask?: (taskId: string, sessionId?: string | null) => boolean;
    getActiveTask?: () => { taskId: string | null; sessionId: string | null };
    getVcsSnapshot?: (worktreeId: string) => WorktreeVcsSnapshot | null;
    refreshVcsDetails?: (worktreeId: string) => boolean;
    toggleDiffPane?: () => boolean;
    toggleArtifactsPane?: () => boolean;
    pasteImageIntoComposer?: (
      selector: string,
      options?: WorkbenchE2EPasteImageOptions,
    ) => Promise<WorkbenchE2EDispatchResult>;
    emitDesktopDrop?: (
      selector: string,
      filePath: string,
    ) => Promise<WorkbenchE2EDispatchResult>;
    measureTargets?: (selectors: Record<string, string>) => Promise<WorkbenchE2EMeasureTargetsResult>;
    measureHarnessOption?: (label: string) => Promise<WorkbenchE2EMeasuredTargetResult | null>;
    measureDiffFile?: (targetPath: string) => Promise<WorkbenchE2EMeasuredTargetResult | null>;
    measureMarkdownParity?: (
      samples: readonly WorkbenchMarkdownParitySample[],
      width: number,
    ) => Promise<WorkbenchMarkdownParityMeasurement[]>;
    measureMarkdownParityDebug?: (
      markdown: string,
      width: number,
      target?: string,
    ) => Promise<WorkbenchMarkdownParityDebugMeasurement>;
    measureMessageParity?: (params: WorkbenchMessageParityParams) => Promise<WorkbenchRowParityMeasurement>;
    measureAssistantParity?: (params: WorkbenchAssistantParityParams) => Promise<WorkbenchRowParityMeasurement>;
    measureAssistantStreamingParity?: (
      params: WorkbenchAssistantStreamingParityParams,
    ) => Promise<WorkbenchAssistantStreamingParityMeasurement>;
    measureTurnHeaderParity?: (params: WorkbenchTurnHeaderParityParams) => Promise<WorkbenchRowParityMeasurement>;
    measureMarkdownSelectionText?: (markdown: string, width: number) => Promise<string>;
    installMarkdownScrollProbe?: (markdown: string, width?: number) => Promise<boolean>;
    removeMarkdownScrollProbe?: () => boolean;
  };
};

type WorkbenchE2EBridgeOptions = {
  focusNewTask: () => void;
  clearDraftHarness: () => void;
  focusTask: (taskId: string, sessionId?: string | null) => boolean;
  getActiveTask: () => { taskId: string | null; sessionId: string | null };
  getVcsSnapshot?: (worktreeId: string) => WorktreeVcsSnapshot | null;
  refreshVcsDetails?: (worktreeId: string) => boolean;
  toggleDiffPane: () => void;
  toggleArtifactsPane: () => void;
};

export function useWorkbenchE2EBridge({
  focusNewTask,
  clearDraftHarness,
  focusTask,
  getActiveTask,
  getVcsSnapshot,
  refreshVcsDetails,
  toggleDiffPane,
  toggleArtifactsPane,
}: WorkbenchE2EBridgeOptions) {
  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const enabled = window.sessionStorage.getItem("ctxE2E") === "1" || params.get("ctxE2E") === "1";
    if (!enabled) return;

    const win = window as WorkbenchE2EWindow;
    win.__ctxE2E ??= {};
    win.__ctxE2E.focusNewTask = () => {
      focusNewTask();
      return true;
    };
    win.__ctxE2E.clearDraftHarness = () => {
      clearDraftHarness();
      return true;
    };
    win.__ctxE2E.focusTask = (taskId: string, sessionId?: string | null) => focusTask(taskId, sessionId);
    win.__ctxE2E.getActiveTask = () => getActiveTask();
    if (getVcsSnapshot) {
      win.__ctxE2E.getVcsSnapshot = (worktreeId: string) => getVcsSnapshot(worktreeId);
    }
    if (refreshVcsDetails) {
      win.__ctxE2E.refreshVcsDetails = (worktreeId: string) => refreshVcsDetails(worktreeId);
    }
    win.__ctxE2E.toggleDiffPane = () => {
      toggleDiffPane();
      return true;
    };
    win.__ctxE2E.toggleArtifactsPane = () => {
      toggleArtifactsPane();
      return true;
    };
    win.__ctxE2E.pasteImageIntoComposer = async (
      selector: string,
      options?: WorkbenchE2EPasteImageOptions,
    ) => {
      const target = document.querySelector(selector);
      if (!(target instanceof HTMLElement)) {
        return { ok: false, error: `missing target: ${selector}` };
      }
      const transfer = makeClipboardTransfer(options);
      target.focus();
      target.dispatchEvent(makePasteEvent(transfer));
      return { ok: true };
    };
    win.__ctxE2E.emitDesktopDrop = async (selector: string, filePath: string) => {
      const target = document.querySelector(selector);
      if (!(target instanceof HTMLElement)) {
        return { ok: false, error: `missing target: ${selector}` };
      }
      const rect = target.getBoundingClientRect();
      const ratio = window.devicePixelRatio > 0 ? window.devicePixelRatio : 1;
      const position = {
        x: Math.round((rect.left + rect.width / 2) * ratio),
        y: Math.round((rect.top + rect.height / 2) * ratio),
      };
      const payload = {
        paths: [String(filePath)],
        position,
      };
      const tauriEmit = (window as Window & {
        __TAURI__?: {
          event?: {
            emit?: (eventName: string, payload: unknown) => Promise<void>;
          };
        };
      }).__TAURI__?.event?.emit;
      const emit = async (eventPayload: object) => {
        if (typeof tauriEmit === "function") {
          await tauriEmit(E2E_DESKTOP_DRAG_DROP_TEST_EVENT, eventPayload);
          return;
        }
        window.dispatchEvent(new CustomEvent(E2E_DESKTOP_DRAG_DROP_TEST_EVENT, { detail: eventPayload }));
      };
      await emit({ type: "enter", ...payload });
      await emit({ type: "over", position });
      await emit({ type: "drop", ...payload });
      return { ok: true };
    };
    win.__ctxE2E.measureTargets = (selectors: Record<string, string>) => measureWorkbenchTargets(selectors);
    win.__ctxE2E.measureHarnessOption = (label: string) => measureWorkbenchHarnessOption(label);
    win.__ctxE2E.measureDiffFile = (targetPath: string) => measureWorkbenchDiffFile(targetPath);
    win.__ctxE2E.measureMarkdownParity = (
      samples: readonly WorkbenchMarkdownParitySample[],
      width: number,
    ) => measureWorkbenchMarkdownParity(samples, width);
    win.__ctxE2E.measureMarkdownParityDebug = (markdown: string, width: number, target?: string) =>
      measureWorkbenchMarkdownParityDebug(markdown, width, target);
    win.__ctxE2E.measureMessageParity = (params: WorkbenchMessageParityParams) => measureWorkbenchMessageParity(params);
    win.__ctxE2E.measureAssistantParity = (params: WorkbenchAssistantParityParams) =>
      measureWorkbenchAssistantParity(params);
    win.__ctxE2E.measureAssistantStreamingParity = (params: WorkbenchAssistantStreamingParityParams) =>
      measureWorkbenchAssistantStreamingParity(params);
    win.__ctxE2E.measureTurnHeaderParity = (params: WorkbenchTurnHeaderParityParams) =>
      measureWorkbenchTurnHeaderParity(params);
    win.__ctxE2E.measureMarkdownSelectionText = (markdown: string, width: number) =>
      measureWorkbenchMarkdownSelectionText(markdown, width);
    win.__ctxE2E.installMarkdownScrollProbe = (markdown: string, width?: number) =>
      installWorkbenchMarkdownScrollProbe(markdown, width);
    win.__ctxE2E.removeMarkdownScrollProbe = () => {
      removeWorkbenchMarkdownScrollProbe();
      return true;
    };

    return () => {
      if (!win.__ctxE2E) return;
      delete win.__ctxE2E.focusNewTask;
      delete win.__ctxE2E.clearDraftHarness;
      delete win.__ctxE2E.focusTask;
      delete win.__ctxE2E.getActiveTask;
      delete win.__ctxE2E.getVcsSnapshot;
      delete win.__ctxE2E.refreshVcsDetails;
      delete win.__ctxE2E.toggleDiffPane;
      delete win.__ctxE2E.toggleArtifactsPane;
      delete win.__ctxE2E.pasteImageIntoComposer;
      delete win.__ctxE2E.emitDesktopDrop;
      delete win.__ctxE2E.measureTargets;
      delete win.__ctxE2E.measureHarnessOption;
      delete win.__ctxE2E.measureDiffFile;
      delete win.__ctxE2E.measureMarkdownParity;
      delete win.__ctxE2E.measureMarkdownParityDebug;
      delete win.__ctxE2E.measureMessageParity;
      delete win.__ctxE2E.measureAssistantParity;
      delete win.__ctxE2E.measureAssistantStreamingParity;
      delete win.__ctxE2E.measureTurnHeaderParity;
      delete win.__ctxE2E.measureMarkdownSelectionText;
      delete win.__ctxE2E.installMarkdownScrollProbe;
      delete win.__ctxE2E.removeMarkdownScrollProbe;
    };
  }, [
    clearDraftHarness,
    focusNewTask,
    focusTask,
    getActiveTask,
    getVcsSnapshot,
    refreshVcsDetails,
    toggleArtifactsPane,
    toggleDiffPane,
  ]);
}
