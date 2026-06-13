import { render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { WorktreeVcsSnapshot } from "@ctx/types";
import { useWorkbenchE2EBridge } from "./useWorkbenchE2EBridge";

vi.mock("../../utils/desktop", () => ({
  desktopGetViewGeometry: vi.fn(async () => ({
    scaleFactor: 2,
    devicePixelRatio: 2,
    webviewPosition: { x: 0, y: 0 },
    webviewSize: { width: 1728, height: 994 },
    windowInnerPosition: { x: 0, y: 66 },
    windowOuterPosition: { x: 0, y: 66 },
    windowInnerSize: { width: 1728, height: 994 },
    windowOuterSize: { width: 1728, height: 994 },
    screenWidth: 1728,
    screenHeight: 1117,
    innerWidth: 1728,
    innerHeight: 962,
  })),
}));

type E2EWindow = Window & {
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
      options?: { base64?: string; htmlSrc?: string; includeFile?: boolean; text?: string },
    ) => Promise<{ ok: boolean; error?: string }>;
    emitDesktopDrop?: (
      selector: string,
      filePath: string,
    ) => Promise<{ ok: boolean; error?: string }>;
    measureTargets?: (selectors: Record<string, string>) => Promise<unknown>;
    measureHarnessOption?: (label: string) => Promise<unknown>;
    measureDiffFile?: (targetPath: string) => Promise<unknown>;
    measureMarkdownParity?: (samples: readonly { name: string; markdown: string }[], width: number) => Promise<unknown>;
    measureMessageParity?: (params: {
      content: string;
      expanded: boolean;
      attachments?: readonly unknown[];
      viewportWidth?: number;
    }) => Promise<unknown>;
    measureAssistantParity?: (params: { content: string; isComplete?: boolean; viewportWidth?: number }) => Promise<unknown>;
    measureAssistantStreamingParity?: (params: { fragments: readonly string[]; viewportWidth?: number }) => Promise<unknown>;
    measureTurnHeaderParity?: (params: { content: string; viewportWidth?: number }) => Promise<unknown>;
    installMarkdownScrollProbe?: (markdown: string, width?: number) => Promise<boolean>;
    removeMarkdownScrollProbe?: () => boolean;
  };
};

function TestBridge(props: {
  focusNewTask: () => void;
  clearDraftHarness: () => void;
  focusTask: (taskId: string, sessionId?: string | null) => boolean;
  getActiveTask: () => { taskId: string | null; sessionId: string | null };
  getVcsSnapshot?: (worktreeId: string) => WorktreeVcsSnapshot | null;
  refreshVcsDetails?: (worktreeId: string) => boolean;
  toggleDiffPane: () => void;
  toggleArtifactsPane: () => void;
}) {
  useWorkbenchE2EBridge(props);
  return null;
}

describe("useWorkbenchE2EBridge", () => {
  const originalDevicePixelRatio = window.devicePixelRatio;

  afterEach(() => {
    sessionStorage.clear();
    delete (window as E2EWindow).__ctxE2E;
    document.body.innerHTML = "";
    Object.defineProperty(window, "devicePixelRatio", { configurable: true, value: originalDevicePixelRatio });
  });

  it("registers diff and artifacts toggles when ctxE2E mode is enabled", () => {
    sessionStorage.setItem("ctxE2E", "1");
    const focusNewTask = vi.fn();
    const clearDraftHarness = vi.fn();
    const focusTask = vi.fn(() => true);
    const getActiveTask = vi.fn(() => ({ taskId: "task-live", sessionId: "session-live" }));
    const getVcsSnapshot = vi.fn((worktreeId: string) =>
      ({ worktree_id: worktreeId, rev: 7 }) as unknown as WorktreeVcsSnapshot,
    );
    const refreshVcsDetails = vi.fn(() => true);
    const toggleDiffPane = vi.fn();
    const toggleArtifactsPane = vi.fn();
    const e2eWindow = window as E2EWindow;

    const view = render(
      <TestBridge
        focusNewTask={focusNewTask}
        clearDraftHarness={clearDraftHarness}
        focusTask={focusTask}
        getActiveTask={getActiveTask}
        getVcsSnapshot={getVcsSnapshot}
        refreshVcsDetails={refreshVcsDetails}
        toggleDiffPane={toggleDiffPane}
        toggleArtifactsPane={toggleArtifactsPane}
      />,
    );

    expect(typeof e2eWindow.__ctxE2E?.focusNewTask).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.clearDraftHarness).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.focusTask).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.getActiveTask).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.getVcsSnapshot).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.refreshVcsDetails).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.toggleDiffPane).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.toggleArtifactsPane).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.pasteImageIntoComposer).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.emitDesktopDrop).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.measureTargets).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.measureHarnessOption).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.measureDiffFile).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.measureMarkdownParity).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.measureMessageParity).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.measureAssistantParity).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.measureAssistantStreamingParity).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.measureTurnHeaderParity).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.installMarkdownScrollProbe).toBe("function");
    expect(typeof e2eWindow.__ctxE2E?.removeMarkdownScrollProbe).toBe("function");
    expect(e2eWindow.__ctxE2E?.focusNewTask?.()).toBe(true);
    expect(e2eWindow.__ctxE2E?.clearDraftHarness?.()).toBe(true);
    expect(e2eWindow.__ctxE2E?.focusTask?.("task-1", "session-1")).toBe(true);
    expect(e2eWindow.__ctxE2E?.getActiveTask?.()).toEqual({ taskId: "task-live", sessionId: "session-live" });
    expect(e2eWindow.__ctxE2E?.getVcsSnapshot?.("worktree-live")).toEqual({ worktree_id: "worktree-live", rev: 7 });
    expect(e2eWindow.__ctxE2E?.refreshVcsDetails?.("worktree-live")).toBe(true);
    expect(e2eWindow.__ctxE2E?.toggleDiffPane?.()).toBe(true);
    expect(e2eWindow.__ctxE2E?.toggleArtifactsPane?.()).toBe(true);
    expect(focusNewTask).toHaveBeenCalledTimes(1);
    expect(clearDraftHarness).toHaveBeenCalledTimes(1);
    expect(focusTask).toHaveBeenCalledWith("task-1", "session-1");
    expect(getActiveTask).toHaveBeenCalledTimes(1);
    expect(getVcsSnapshot).toHaveBeenCalledWith("worktree-live");
    expect(refreshVcsDetails).toHaveBeenCalledWith("worktree-live");
    expect(toggleDiffPane).toHaveBeenCalledTimes(1);
    expect(toggleArtifactsPane).toHaveBeenCalledTimes(1);

    view.unmount();
    expect(e2eWindow.__ctxE2E?.focusNewTask).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.clearDraftHarness).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.focusTask).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.getActiveTask).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.getVcsSnapshot).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.refreshVcsDetails).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.toggleDiffPane).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.toggleArtifactsPane).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.pasteImageIntoComposer).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.emitDesktopDrop).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.measureTargets).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.measureHarnessOption).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.measureDiffFile).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.measureMarkdownParity).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.measureMessageParity).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.measureAssistantParity).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.measureAssistantStreamingParity).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.measureTurnHeaderParity).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.installMarkdownScrollProbe).toBeUndefined();
    expect(e2eWindow.__ctxE2E?.removeMarkdownScrollProbe).toBeUndefined();
  });

  it("dispatches paste and desktop drop events from the app realm", async () => {
    Object.defineProperty(window, "devicePixelRatio", { configurable: true, value: 2 });

    sessionStorage.setItem("ctxE2E", "1");
    const e2eWindow = window as E2EWindow;
    render(
      <TestBridge
        focusNewTask={() => {}}
        clearDraftHarness={() => {}}
        focusTask={() => true}
        getActiveTask={() => ({ taskId: null, sessionId: null })}
        toggleDiffPane={() => {}}
        toggleArtifactsPane={() => {}}
      />,
    );

    const textarea = document.createElement("textarea");
    textarea.className = "wb-new-composer-textarea";
    document.body.appendChild(textarea);

    let pastedFileCount = 0;
    let pastedText = "";
    textarea.addEventListener("paste", (event) => {
      const clipboardData = (event as unknown as {
        clipboardData?: {
          files: File[];
          items: Array<{ kind: string; getAsFile: () => File }>;
          types: string[];
          getData: (type: string) => string;
        };
      }).clipboardData;
      pastedFileCount = clipboardData?.files.length ?? 0;
      pastedText = clipboardData?.getData("text/plain") ?? "";
      expect(clipboardData?.items[0]?.kind).toBe("file");
      expect(clipboardData?.types).toContain("Files");
    });

    await expect(
      e2eWindow.__ctxE2E?.pasteImageIntoComposer?.("textarea.wb-new-composer-textarea", { text: "caption" }),
    ).resolves.toEqual({ ok: true });
    expect(pastedFileCount).toBe(1);
    expect(pastedText).toBe("caption");

    const dropScope = document.createElement("div");
    dropScope.className = "ctx-drop-scope";
    dropScope.getBoundingClientRect = vi.fn(() => ({
      left: 10,
      top: 20,
      width: 100,
      height: 40,
      right: 110,
      bottom: 60,
      x: 10,
      y: 20,
      toJSON: () => ({}),
    }));
    document.body.appendChild(dropScope);

    const emittedDrops: unknown[] = [];
    window.addEventListener("ctx:desktop-drag-drop-test", (event) => {
      emittedDrops.push((event as CustomEvent).detail);
    });

    await expect(e2eWindow.__ctxE2E?.emitDesktopDrop?.(".ctx-drop-scope", "/tmp/paste.png")).resolves.toEqual({ ok: true });
    expect(emittedDrops).toEqual([
      { type: "enter", paths: ["/tmp/paste.png"], position: { x: 120, y: 80 } },
      { type: "over", position: { x: 120, y: 80 } },
      { type: "drop", paths: ["/tmp/paste.png"], position: { x: 120, y: 80 } },
    ]);
  });
});
