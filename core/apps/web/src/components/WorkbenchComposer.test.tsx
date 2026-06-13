import { act, createEvent, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { useEffect, useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WorkbenchComposer } from "./WorkbenchComposer";
import type { MessageAttachment, ProviderOptions, ProviderStatus } from "../api/client";
import { resetBrowserResourceUrlCacheForTests } from "../api/browserResourceUrls";
import {
  resetDaemonConnectionStateForTests,
  setDaemonConnection,
} from "../api/daemonConnection";
import type { DraftHarness, WorkbenchComposerProps, WorkbenchModeId } from "./WorkbenchComposer";
import type { HarnessCatalogEntry } from "../utils/harnessCatalog";

type NewSessionProps = Extract<WorkbenchComposerProps, { variant: "newSession" }>;

const { trackFeatureUsedMock, trackProviderSelectedMock } = vi.hoisted(() => ({
  trackFeatureUsedMock: vi.fn(),
  trackProviderSelectedMock: vi.fn(),
}));
const uploadBlobMock = vi.hoisted(() =>
  vi.fn(async (file: File) => ({
    blob_id: `blob-${file.name || "image"}`,
    mime_type: file.type || "image/png",
    name: file.name || "image.png",
  })),
);

vi.mock("../utils/analytics", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../utils/analytics")>();
  return {
    ...actual,
    trackFeatureUsed: trackFeatureUsedMock,
    trackProviderSelected: trackProviderSelectedMock,
  };
});

vi.mock("../api/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/client")>();
  return {
    ...actual,
    uploadBlob: uploadBlobMock,
  };
});

function mockRaf() {
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((cb: FrameRequestCallback) => {
    return window.setTimeout(() => cb(performance.now()), 0) as unknown as number;
  });
}

function baseOptions(providerId: string): ProviderOptions {
  return {
    provider_id: providerId,
    workspace_id: "ws-test",
    supports_load: false,
    auth_required: false,
    probed_at: new Date().toISOString(),
  };
}

function makeProviderStatus(providerId: string, overrides: Partial<ProviderStatus> = {}): ProviderStatus {
  return {
    provider_id: providerId,
    installed: true,
    health: "ok",
    diagnostics: [],
    details: {},
    usability: {
      usable: true,
      status: "ready",
      blocking_provider_ids: [],
      recommended_action: "none",
    },
    ...overrides,
  };
}

function expectBrowserTextAssistsDisabled(element: HTMLElement) {
  expect(element).toHaveAttribute("autocomplete", "off");
  expect(element).toHaveAttribute("autocorrect", "off");
  expect(element).toHaveAttribute("autocapitalize", "none");
  expect(element).toHaveAttribute("spellcheck", "false");
}

describe("WorkbenchComposer textarea sizing", () => {
  type SubmitSnapshot = {
    activeTag: string | null;
    selectionStart: number | null;
    selectionEnd: number | null;
    selectedText: string | null;
    globalSelection: string;
  };

  const originalScrollHeight = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "scrollHeight");

  beforeEach(() => {
    resetBrowserResourceUrlCacheForTests();
    setDaemonConnection({
      baseUrl: "http://daemon.test",
      authToken: "daemon-secret",
      source: "test",
      mobileSecure: null,
    });
    mockRaf();
    Object.defineProperty(HTMLTextAreaElement.prototype, "scrollHeight", {
      configurable: true,
      get() {
        const lines = String((this as HTMLTextAreaElement).value ?? "").split("\n").length;
        const contentHeight = Math.max(1, lines) * 20;
        const styleHeight = Number.parseFloat((this as HTMLTextAreaElement).style.height || "0");
        return Math.max(contentHeight, Number.isFinite(styleHeight) ? styleHeight : 0);
      },
    });
  });

  afterEach(() => {
    resetBrowserResourceUrlCacheForTests();
    resetDaemonConnectionStateForTests();
    vi.restoreAllMocks();
    trackFeatureUsedMock.mockReset();
    trackProviderSelectedMock.mockReset();
    uploadBlobMock.mockImplementation(async (file: File) => ({
      blob_id: `blob-${file.name || "image"}`,
      mime_type: file.type || "image/png",
      name: file.name || "image.png",
    }));
    if (originalScrollHeight) {
      Object.defineProperty(HTMLTextAreaElement.prototype, "scrollHeight", originalScrollHeight);
    } else {
      Object.defineProperty(HTMLTextAreaElement.prototype, "scrollHeight", {
        configurable: true,
        get() {
          return 0;
        },
      });
    }
  });

  it("auto-resizes the new task composer up to 380px", async () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "o3" });
      const harnessCatalog: HarnessCatalogEntry[] = [{ id: "codex", label: "Codex", logoSrc: "" }];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
      };
      const providerOptions: Record<string, ProviderOptions | undefined> = {};

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={providerOptions}
          ensureProviderAuthSummary={async () => undefined}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    const textarea = screen.getByPlaceholderText("@ for context, / for commands") as HTMLTextAreaElement;

    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(textarea.style.height).toBe("88px");

    await act(async () => {
      fireEvent.change(textarea, { target: { value: ["a", "b", "c", "d", "e"].join("\n") } });
      await new Promise((r) => setTimeout(r, 0));
    });
    const expandedHeight = Number.parseFloat(textarea.style.height || "0");
    expect(expandedHeight).toBeGreaterThan(88);
    expect(expandedHeight).toBeLessThanOrEqual(380);

    const manyLines = Array.from({ length: 30 }, (_, i) => `line ${i + 1}`).join("\n");
    await act(async () => {
      fireEvent.change(textarea, { target: { value: manyLines } });
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(textarea.style.height).toBe("380px");
  });

  it("disables browser text assistance on the composer textarea", () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "o3" });
      const harnessCatalog: HarnessCatalogEntry[] = [{ id: "codex", label: "Codex", logoSrc: "" }];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={{}}
          ensureProviderAuthSummary={async () => undefined}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);

    expectBrowserTextAssistsDisabled(screen.getByPlaceholderText("@ for context, / for commands"));
  });

  it("avoids zeroing the textarea height during resize", async () => {
    const ActiveHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");

      return (
        <WorkbenchComposer
          variant="activeSession"
          value={value}
          setValue={setValue}
          placeholder="Ask follow-ups"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          recording={false}
          harnessLabel="Codex"
          availableModels={[{ id: "o3", name: "o3" }]}
          currentModelId="o3"
          onSetModelId={vi.fn()}
        />
      );
    };

    render(<ActiveHarness />);
    const textarea = screen.getByPlaceholderText("Ask follow-ups") as HTMLTextAreaElement;

    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });

    const heights: string[] = [];
    const style = textarea.style;
    const originalHeightDescriptor = Object.getOwnPropertyDescriptor(style, "height");
    let storedHeight = style.height;
    Object.defineProperty(style, "height", {
      configurable: true,
      get() {
        return storedHeight;
      },
      set(value) {
        const next = String(value);
        heights.push(next);
        storedHeight = next;
      },
    });

    await act(async () => {
      fireEvent.change(textarea, { target: { value: "line 1\nline 2\nline 3" } });
      await new Promise((r) => setTimeout(r, 0));
    });

    if (originalHeightDescriptor) {
      Object.defineProperty(style, "height", originalHeightDescriptor);
    }

    expect(heights).not.toContain("0px");
  });

  it("does not collapse the textarea when typing multi-line input", async () => {
    const ActiveHarness = () => {
      const [value, setValue] = useState("line 1\nline 2\nline 3");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");

      return (
        <WorkbenchComposer
          variant="activeSession"
          value={value}
          setValue={setValue}
          placeholder="Ask follow-ups"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          recording={false}
          harnessLabel="Codex"
          availableModels={[{ id: "o3", name: "o3" }]}
          currentModelId="o3"
          onSetModelId={vi.fn()}
        />
      );
    };

    render(<ActiveHarness />);
    const textarea = screen.getByPlaceholderText("Ask follow-ups") as HTMLTextAreaElement;

    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });

    const initialHeight = Number.parseFloat(textarea.style.height || "0");
    expect(initialHeight).toBeGreaterThan(0);

    const heightWrites: string[] = [];
    const style = textarea.style;
    const originalHeightDescriptor = Object.getOwnPropertyDescriptor(style, "height");
    let storedHeight = style.height;
    Object.defineProperty(style, "height", {
      configurable: true,
      get() {
        return storedHeight;
      },
      set(value) {
        const next = String(value);
        heightWrites.push(next);
        storedHeight = next;
      },
    });

    await act(async () => {
      fireEvent.change(textarea, { target: { value: `${textarea.value}x` } });
      await new Promise((r) => setTimeout(r, 0));
    });

    if (originalHeightDescriptor) {
      Object.defineProperty(style, "height", originalHeightDescriptor);
    }

    expect(heightWrites).not.toContain("auto");
    const collapsed = heightWrites.some((next) => {
      const numeric = Number.parseFloat(next);
      return Number.isFinite(numeric) && numeric < initialHeight - 0.5;
    });
    expect(collapsed).toBe(false);
  });

  it("prefers the resolved active-session model slug when provided", () => {
    const ActiveHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");

      return (
        <WorkbenchComposer
          variant="activeSession"
          value={value}
          setValue={setValue}
          placeholder="Ask follow-ups"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          recording={false}
          harnessLabel="Claude"
          availableModels={[{ id: "opus/high", name: "Opus 4.7 (High)" }]}
          currentModelId="opus/high"
          currentModelDisplayLabel="Opus 4.7"
          onSetModelId={vi.fn()}
        />
      );
    };

    render(<ActiveHarness />);

    expect(screen.getByRole("button", { name: /opus 4.7/i })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^opus$/i })).not.toBeInTheDocument();
  });

  it("resets to the minimum height after clearing content", async () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "o3" });
      const harnessCatalog: HarnessCatalogEntry[] = [{ id: "codex", label: "Codex", logoSrc: "" }];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
      };
      const providerOptions: Record<string, ProviderOptions | undefined> = {};

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={providerOptions}
          ensureProviderAuthSummary={async () => undefined}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    const textarea = screen.getByPlaceholderText("@ for context, / for commands") as HTMLTextAreaElement;

    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });

    await act(async () => {
      fireEvent.change(textarea, { target: { value: "line 1\nline 2\nline 3\nline 4\nline 5" } });
      await new Promise((r) => setTimeout(r, 0));
    });
    const expandedHeight = Number.parseFloat(textarea.style.height || "0");
    expect(expandedHeight).toBeGreaterThan(88);

    await act(async () => {
      fireEvent.change(textarea, { target: { value: "" } });
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(textarea.style.height).toBe("88px");

    await act(async () => {
      fireEvent.change(textarea, { target: { value: "a" } });
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(textarea.style.height).toBe("88px");
  });

  it("collapses new-task selection before sending on Enter", async () => {
    let submitSnapshot: SubmitSnapshot | null = null;

    const NewTaskHarness = () => {
      const [value, setValue] = useState("alpha beta gamma");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "o3" });
      const harnessCatalog: HarnessCatalogEntry[] = [{ id: "codex", label: "Codex", logoSrc: "" }];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={() => {
            const textarea = document.querySelector("textarea.wb-new-composer-textarea") as HTMLTextAreaElement | null;
            submitSnapshot = {
              activeTag: document.activeElement?.tagName ?? null,
              selectionStart: textarea?.selectionStart ?? null,
              selectionEnd: textarea?.selectionEnd ?? null,
              selectedText:
                textarea && textarea.selectionStart != null && textarea.selectionEnd != null
                  ? textarea.value.slice(textarea.selectionStart, textarea.selectionEnd)
                  : null,
              globalSelection: window.getSelection()?.toString() ?? "",
            };
          }}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={{}}
          ensureProviderAuthSummary={async () => undefined}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    const textarea = screen.getByPlaceholderText("@ for context, / for commands") as HTMLTextAreaElement;

    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });

    await act(async () => {
      textarea.focus();
      const start = textarea.value.indexOf("beta");
      const end = start + "beta".length;
      textarea.setSelectionRange(start, end);
      fireEvent.select(textarea);
      await new Promise((r) => setTimeout(r, 0));
    });

    expect(textarea.value.slice(textarea.selectionStart ?? 0, textarea.selectionEnd ?? 0)).toBe("beta");

    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter", code: "Enter" });
      await new Promise((r) => setTimeout(r, 0));
    });

    expect(submitSnapshot).not.toBeNull();
    if (!submitSnapshot) {
      throw new Error("expected submit snapshot");
    }
    const snapshot = submitSnapshot as SubmitSnapshot;
    expect(snapshot.selectionStart).toBe(snapshot.selectionEnd);
    expect(snapshot.selectedText).toBe("");
    expect(snapshot.globalSelection).toBe("");
    expect(snapshot.activeTag).not.toBe("TEXTAREA");
  });

  it("keeps the textarea scrolled to bottom while recording", async () => {
    const ActiveHarness = () => {
      const [value, setValue] = useState(Array.from({ length: 25 }, (_, i) => `line ${i + 1}`).join("\n"));
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");

      return (
        <WorkbenchComposer
          variant="activeSession"
          value={value}
          setValue={setValue}
          placeholder="Ask follow-ups"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          recording={true}
          harnessLabel="Codex"
          availableModels={[{ id: "o3", name: "o3" }]}
          currentModelId="o3"
          onSetModelId={vi.fn()}
        />
      );
    };

    render(<ActiveHarness />);
    const textarea = screen.getByPlaceholderText("Ask follow-ups") as HTMLTextAreaElement;

    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });

    textarea.scrollTop = 0;
    expect(textarea.scrollTop).toBe(0);

    await act(async () => {
      fireEvent.change(textarea, { target: { value: `${textarea.value}\nline 26` } });
      await new Promise((r) => setTimeout(r, 0));
    });

    expect(textarea.scrollTop).toBe(textarea.scrollHeight);
  });

  it("restores hydrated drafts with the cursor and scroll position at the end", async () => {
    const hydratedValue = Array.from({ length: 25 }, (_, i) => `line ${i + 1}`).join("\n");

    const ActiveHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");

      useEffect(() => {
        const timer = window.setTimeout(() => setValue(hydratedValue), 0);
        return () => window.clearTimeout(timer);
      }, []);

      return (
        <WorkbenchComposer
          variant="activeSession"
          value={value}
          setValue={setValue}
          placeholder="Ask follow-ups"
          inputDisabled={false}
          sessionIdForAutocomplete="session-1"
          workspaceIdForAutocomplete="ws-1"
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          isWorking={false}
          modeId={modeId}
          setModeId={setModeId}
          harnessLabel="Codex"
          harnessLogoSrc=""
          harnessLogoInvert={false}
          harnessLogoInvertInLight={false}
          verbosity="default"
          onSetVerbosity={undefined}
          contextWindow={null}
          availableModels={[]}
          currentModelId="gpt-5"
          onSetModelId={vi.fn(async () => {})}
        />
      );
    };

    render(<ActiveHarness />);
    const textarea = screen.getByPlaceholderText("Ask follow-ups") as HTMLTextAreaElement;
    textarea.scrollTop = 0;

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(textarea.value).toBe(hydratedValue);
    expect(textarea.selectionStart).toBe(hydratedValue.length);
    expect(textarea.selectionEnd).toBe(hydratedValue.length);
    expect(textarea.scrollTop).toBe(textarea.scrollHeight);
  });

  it("shows an explicit unknown context-window state when metrics are unavailable", async () => {
    const ActiveHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");

      return (
        <WorkbenchComposer
          variant="activeSession"
          value={value}
          setValue={setValue}
          placeholder="Ask follow-ups"
          inputDisabled={false}
          sessionIdForAutocomplete="session-1"
          workspaceIdForAutocomplete="ws-1"
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          isWorking={false}
          modeId={modeId}
          setModeId={setModeId}
          harnessLabel="Codex"
          harnessLogoSrc=""
          harnessLogoInvert={false}
          harnessLogoInvertInLight={false}
          verbosity="default"
          onSetVerbosity={undefined}
          contextWindow={null}
          availableModels={[]}
          currentModelId="gpt-5"
          onSetModelId={vi.fn(async () => {})}
        />
      );
    };

    render(<ActiveHarness />);

    expect(document.querySelector(".wb-context-window")).toBeNull();
  });

  it("never renders a context-window indicator in the new-task composer", async () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "gpt-5" });
      const harnessCatalog: HarnessCatalogEntry[] = [{ id: "codex", label: "Codex", logoSrc: "" }];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={{}}
          ensureProviderAuthSummary={async () => undefined}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);

    expect(document.querySelector(".wb-context-window")).toBeNull();
  });

  it("renders known context-window usage when canonical metrics are present", async () => {
    const ActiveHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");

      return (
        <WorkbenchComposer
          variant="activeSession"
          value={value}
          setValue={setValue}
          placeholder="Ask follow-ups"
          inputDisabled={false}
          sessionIdForAutocomplete="session-1"
          workspaceIdForAutocomplete="ws-1"
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          isWorking={false}
          modeId={modeId}
          setModeId={setModeId}
          harnessLabel="Codex"
          harnessLogoSrc=""
          harnessLogoInvert={false}
          harnessLogoInvertInLight={false}
          verbosity="default"
          onSetVerbosity={undefined}
          contextWindow={{
            windowTokens: 100,
            usedTokens: 7,
            remainingTokens: 93,
            remainingFraction: 0.93,
          }}
          availableModels={[]}
          currentModelId="gpt-5"
          onSetModelId={vi.fn(async () => {})}
        />
      );
    };

    render(<ActiveHarness />);

    const indicator = screen.getByText("7% · 7/100");
    expect(indicator).toHaveAttribute("title", "Context Window: 7% · 7/100");
  });

  it("renders Codex model labels from lowercase slugs in the active composer", () => {
    const ActiveHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");

      return (
        <WorkbenchComposer
          variant="activeSession"
          value={value}
          setValue={setValue}
          placeholder="Ask follow-ups"
          inputDisabled={false}
          sessionIdForAutocomplete="session-1"
          workspaceIdForAutocomplete="ws-1"
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          isWorking={false}
          modeId={modeId}
          setModeId={setModeId}
          providerId="codex"
          harnessLabel="Agents"
          harnessLogoSrc=""
          harnessLogoInvert={false}
          harnessLogoInvertInLight={false}
          verbosity="default"
          onSetVerbosity={undefined}
          contextWindow={null}
          availableModels={[
            { id: "gpt-5.5/medium", name: "GPT-5.5 (medium)" },
            { id: "gpt-5.4-mini/medium", name: "GPT-5.4-Mini (medium)" },
          ]}
          currentModelId="gpt-5.5/medium"
          currentModelDisplayLabel="GPT-5.5"
          onSetModelId={vi.fn(async () => {})}
        />
      );
    };

    render(<ActiveHarness />);

    const modelButton = screen.getByRole("button", { name: "gpt-5.5" });
    expect(screen.queryByRole("button", { name: "GPT-5.5" })).toBeNull();

    fireEvent.click(modelButton);
    const modelMenu = screen.getByRole("menu", { hidden: true });
    expect(within(modelMenu).getByText("gpt-5.4-mini")).toBeInTheDocument();
    expect(within(modelMenu).queryByText("GPT-5.4-Mini")).toBeNull();
  });

  it("keeps the stop action visible while a turn is active even if draft attachments remain", async () => {
    const onSend = vi.fn();
    const onInterrupt = vi.fn();

    const ActiveHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([
        {
          kind: "image_ref",
          blob_id: "blob-1",
          mime_type: "image/png",
          name: "blob-1.png",
        },
      ]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");

      return (
        <WorkbenchComposer
          variant="activeSession"
          value={value}
          setValue={setValue}
          placeholder="Ask follow-ups"
          inputDisabled={false}
          sessionIdForAutocomplete="session-1"
          workspaceIdForAutocomplete="ws-1"
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={onSend}
          sendDisabled={true}
          sendDisabledReason="Sending..."
          onInterrupt={onInterrupt}
          isWorking={true}
          modeId={modeId}
          setModeId={setModeId}
          harnessLabel="Codex"
          harnessLogoSrc=""
          harnessLogoInvert={false}
          harnessLogoInvertInLight={false}
          verbosity="default"
          onSetVerbosity={undefined}
          contextWindow={null}
          availableModels={[{ id: "gpt-5.4/medium", name: "GPT-5.4 (Medium)" }]}
          currentModelId="gpt-5.4/medium"
          onSetModelId={vi.fn(async () => {})}
        />
      );
    };

    render(<ActiveHarness />);

    const stopButton = screen.getByRole("button", { name: "Stop" });
    expect(stopButton).toBeEnabled();

    fireEvent.click(stopButton);
    expect(onInterrupt).toHaveBeenCalledTimes(1);
    expect(onSend).not.toHaveBeenCalled();
  });

  it("renders an immediate stopping state while interrupt is pending", () => {
    const onInterrupt = vi.fn();

    const ActiveHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");

      return (
        <WorkbenchComposer
          variant="activeSession"
          value={value}
          setValue={setValue}
          placeholder="Ask follow-ups"
          inputDisabled={false}
          sessionIdForAutocomplete="session-1"
          workspaceIdForAutocomplete="ws-1"
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={onInterrupt}
          isWorking={false}
          interruptPending={true}
          modeId={modeId}
          setModeId={setModeId}
          harnessLabel="Codex"
          harnessLogoSrc=""
          harnessLogoInvert={false}
          harnessLogoInvertInLight={false}
          verbosity="default"
          onSetVerbosity={undefined}
          contextWindow={null}
          availableModels={[{ id: "gpt-5.4/medium", name: "GPT-5.4 (Medium)" }]}
          currentModelId="gpt-5.4/medium"
          onSetModelId={vi.fn(async () => {})}
        />
      );
    };

    render(<ActiveHarness />);

    const stopButton = screen.getByRole("button", { name: "Stopping..." });
    expect(stopButton).toBeDisabled();
    expect(stopButton).toHaveAttribute("title", "Stopping...");

    fireEvent.click(stopButton);
    expect(onInterrupt).not.toHaveBeenCalled();
  });

  it("filters unsupported harness ids from the harness menu", async () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "o3" });
      const harnessCatalog: HarnessCatalogEntry[] = [
        { id: "codex", label: "Codex", logoSrc: "" },
        { id: "codebuff", label: "Codebuff", logoSrc: "" },
      ];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
        codebuff: makeProviderStatus("codebuff"),
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={{}}
          ensureProviderAuthSummary={async () => undefined}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    fireEvent.click(screen.getByRole("button", { name: "Codex" }));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(screen.queryByRole("button", { name: /Codebuff/ })).not.toBeInTheDocument();
  });

  it("does not surface dependency-only provider ids in the harness menu", async () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "o3" });
      const harnessCatalog: HarnessCatalogEntry[] = [{ id: "codex", label: "Codex", logoSrc: "" }];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
        "acp-crp-bridge": makeProviderStatus("acp-crp-bridge", {
          details: {
            provider_kind: "dependency",
          },
        }),
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={{}}
          ensureProviderAuthSummary={async () => undefined}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    fireEvent.click(screen.getByRole("button", { name: "Codex" }));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(screen.queryByText("acp-crp-bridge")).not.toBeInTheDocument();
  });

  it("does not treat codex subscription mode without active auth as configured", async () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "o3" });
      const harnessCatalog: HarnessCatalogEntry[] = [{ id: "codex", label: "Codex", logoSrc: "" }];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
      };
      const providerOptions: Record<string, ProviderOptions | undefined> = {
        codex: {
          provider_id: "codex",
          workspace_id: "ws-test",
          supports_load: false,
          auth_required: false,
          has_active_auth: false,
          auth_mode: "subscription",
          probed_at: new Date().toISOString(),
        },
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={providerOptions}
          ensureProviderAuthSummary={async () => providerOptions.codex}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    fireEvent.click(screen.getByRole("button", { name: "Codex" }));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(screen.getByTitle("Authentication not configured")).toBeInTheDocument();
  });

  it("shows an inactive auth dot for installed harnesses without auth", async () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "o3" });
      const harnessCatalog: HarnessCatalogEntry[] = [
        { id: "codex", label: "Codex", logoSrc: "" },
        { id: "cursor", label: "Cursor", logoSrc: "" },
      ];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
        cursor: makeProviderStatus("cursor"),
      };
      const providerOptions: Record<string, ProviderOptions | undefined> = {
        codex: { ...baseOptions("codex"), has_active_auth: true },
        cursor: { ...baseOptions("cursor"), has_active_auth: false },
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={providerOptions}
          ensureProviderAuthSummary={async (providerId: string) => providerOptions[providerId]}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    fireEvent.click(screen.getByRole("button", { name: "Codex" }));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    const cursorRow = screen.getByText("Cursor").closest(".wb-harness-row");
    expect(cursorRow).not.toBeNull();
    expect(within(cursorRow as HTMLElement).getByTitle("Authentication not configured")).toBeInTheDocument();
  });

  it("shows a simple install button without host or container text for uninstalled harnesses", async () => {
    const onInstallProvider = vi.fn();

    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "o3" });
      const harnessCatalog: HarnessCatalogEntry[] = [
        { id: "codex", label: "Codex", logoSrc: "" },
        { id: "cursor", label: "Cursor", logoSrc: "" },
      ];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
        cursor: makeProviderStatus("cursor", {
          installed: false,
          health: "missing",
          usability: {
            usable: false,
            status: "blocked",
            blocking_provider_ids: [],
            recommended_action: "install",
            reason: "not installed",
          },
          details: {
            install_supported: "true",
            install_target: "container",
          },
        }),
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={onInstallProvider}
          onInstallAllProviders={vi.fn()}
          providerOptions={{}}
          ensureProviderAuthSummary={async () => undefined}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    fireEvent.click(screen.getByRole("button", { name: "Codex" }));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    const cursorRow = screen.getByText("Cursor").closest(".wb-harness-row");
    expect(cursorRow).not.toBeNull();
    expect(within(cursorRow as HTMLElement).queryByText(/\b(container|host)\b/i)).not.toBeInTheDocument();
    expect(within(cursorRow as HTMLElement).queryByTitle("Authentication not configured")).not.toBeInTheDocument();
    fireEvent.click(within(cursorRow as HTMLElement).getByRole("button", { name: "Install" }));
    expect(onInstallProvider).toHaveBeenCalledWith("cursor");
  });

  it("shows install progress as a percentage pill without target text", async () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "o3" });
      const harnessCatalog: HarnessCatalogEntry[] = [
        { id: "codex", label: "Codex", logoSrc: "" },
        { id: "cursor", label: "Cursor", logoSrc: "" },
      ];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
        cursor: makeProviderStatus("cursor", {
          installed: false,
          health: "missing",
          usability: {
            usable: false,
            status: "blocked",
            blocking_provider_ids: [],
            recommended_action: "install",
            reason: "not installed",
          },
          details: {
            install_supported: "true",
            install_target: "host",
          },
        }),
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{
            cursor: {
              installId: "install-cursor",
              state: "running",
              pct: 42,
              target: "host",
            },
          }}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={{}}
          ensureProviderAuthSummary={async () => undefined}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    fireEvent.click(screen.getByRole("button", { name: "Codex" }));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    const cursorRow = screen.getByText("Cursor").closest(".wb-harness-row");
    expect(cursorRow).not.toBeNull();
    expect(within(cursorRow as HTMLElement).queryByText(/\b(container|host)\b/i)).not.toBeInTheDocument();
    expect(within(cursorRow as HTMLElement).queryByTitle("Authentication not configured")).not.toBeInTheDocument();
    const progressButton = within(cursorRow as HTMLElement).getByRole("button", { name: "42%" });
    expect(progressButton.className).toContain("wb-harness-install-busy");
  });

  it("shows update actions for installed providers with available runtime updates", async () => {
    const onInstallProvider = vi.fn();

    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "o3" });
      const harnessCatalog: HarnessCatalogEntry[] = [
        { id: "codex", label: "Codex", logoSrc: "" },
        { id: "cursor", label: "Cursor", logoSrc: "" },
      ];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
        cursor: makeProviderStatus("cursor", {
          details: {
            install_supported: "true",
            install_target: "host",
            matrix_update_available: "true",
          },
        }),
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={onInstallProvider}
          onInstallAllProviders={vi.fn()}
          providerOptions={{}}
          ensureProviderAuthSummary={async () => undefined}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    fireEvent.click(screen.getByRole("button", { name: "Codex" }));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    const cursorRow = screen.getByText("Cursor").closest(".wb-harness-row");
    expect(cursorRow).not.toBeNull();
    expect(within(cursorRow as HTMLElement).queryByTitle("Authentication configured")).not.toBeInTheDocument();
    fireEvent.click(within(cursorRow as HTMLElement).getByRole("button", { name: "Update" }));
    expect(onInstallProvider).toHaveBeenCalledWith("cursor");
  });

  it("shows update progress for installed providers while a runtime update is running", async () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "o3" });
      const harnessCatalog: HarnessCatalogEntry[] = [
        { id: "codex", label: "Codex", logoSrc: "" },
        { id: "cursor", label: "Cursor", logoSrc: "" },
      ];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
        cursor: makeProviderStatus("cursor", {
          details: {
            install_supported: "true",
            install_target: "host",
            matrix_update_available: "true",
          },
        }),
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{
            cursor: {
              installId: "install-cursor",
              state: "running",
              pct: 42,
              target: "host",
            },
          }}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={{}}
          ensureProviderAuthSummary={async () => undefined}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    fireEvent.click(screen.getByRole("button", { name: "Codex" }));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    const cursorRow = screen.getByText("Cursor").closest(".wb-harness-row");
    expect(cursorRow).not.toBeNull();
    expect(within(cursorRow as HTMLElement).queryByTitle("Authentication configured")).not.toBeInTheDocument();
    const progressButton = within(cursorRow as HTMLElement).getByRole("button", { name: "42%" });
    expect(progressButton.className).toContain("wb-harness-install-busy");
  });

  it("does not revert to Update after a runtime update finishes successfully", async () => {
    const onInstallProvider = vi.fn();
    const harnessCatalog: HarnessCatalogEntry[] = [
      { id: "codex", label: "Codex", logoSrc: "" },
      { id: "cursor", label: "Cursor", logoSrc: "" },
    ];
    const renderHarness = (
      providersById: Record<string, ProviderStatus>,
      providerInstallsById: NewSessionProps["providerInstallsById"],
    ) => (
      <WorkbenchComposer
        variant="newSession"
        value=""
        setValue={vi.fn()}
        placeholder="@ for context, / for commands"
        inputDisabled={false}
        sessionIdForAutocomplete={null}
        workspaceIdForAutocomplete={null}
        slashCommands={[]}
        attachments={[]}
        setAttachments={vi.fn()}
        onSend={vi.fn()}
        sendDisabled={false}
        sendDisabledReason={null}
        onInterrupt={null}
        modeId="default"
        setModeId={vi.fn()}
        harnessCatalog={harnessCatalog}
        providersById={providersById}
        providerInstallsById={providerInstallsById}
        onInstallProvider={onInstallProvider}
        onInstallAllProviders={vi.fn()}
        providerOptions={{}}
        ensureProviderAuthSummary={async () => undefined}
        draftHarness={{ providerId: "codex", modelId: "o3" }}
        setDraftHarness={vi.fn()}
        defaultProviderId="codex"
      />
    );

    const updatingProviders: Record<string, ProviderStatus> = {
      codex: makeProviderStatus("codex"),
      cursor: makeProviderStatus("cursor", {
        details: {
          install_supported: "true",
          install_target: "host",
          matrix_update_available: "true",
        },
      }),
    };

    const { rerender } = render(
      renderHarness(updatingProviders, {}),
    );

    fireEvent.click(screen.getByRole("button", { name: "Codex" }));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const initialCursorRow = screen.getByText("Cursor").closest(".wb-harness-row");
    expect(initialCursorRow).not.toBeNull();
    fireEvent.click(within(initialCursorRow as HTMLElement).getByRole("button", { name: "Update" }));
    expect(onInstallProvider).toHaveBeenCalledWith("cursor");

    rerender(renderHarness(
      updatingProviders,
      {
        cursor: {
          installId: "install-cursor",
          state: "running",
          pct: 42,
          target: "host",
        },
      },
    ));

    const progressCursorRow = screen.getByText("Cursor").closest(".wb-harness-row");
    expect(progressCursorRow).not.toBeNull();
    expect(within(progressCursorRow as HTMLElement).getByRole("button", { name: "42%" })).toBeInTheDocument();

    rerender(renderHarness(
      {
        codex: makeProviderStatus("codex"),
        cursor: makeProviderStatus("cursor", {
          details: {
            install_supported: "true",
            install_target: "host",
          },
        }),
      },
      {},
    ));

    const finishedCursorRow = screen.getByText("Cursor").closest(".wb-harness-row");
    expect(finishedCursorRow).not.toBeNull();
    expect(within(finishedCursorRow as HTMLElement).queryByRole("button", { name: "Update" })).not.toBeInTheDocument();
    expect(within(finishedCursorRow as HTMLElement).queryByRole("button", { name: "42%" })).not.toBeInTheDocument();
  });

  it("shows finalizing state instead of 100% after install success until the provider is ready", async () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "o3" });
      const harnessCatalog: HarnessCatalogEntry[] = [
        { id: "codex", label: "Codex", logoSrc: "" },
        { id: "cursor", label: "Cursor", logoSrc: "" },
      ];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
        cursor: makeProviderStatus("cursor", {
          installed: false,
          health: "missing",
          usability: {
            usable: false,
            status: "blocked",
            blocking_provider_ids: [],
            recommended_action: "install",
            reason: "not installed",
          },
          details: {
            install_supported: "true",
            install_target: "host",
          },
        }),
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{
            cursor: {
              installId: "install-cursor",
              state: "succeeded",
              pct: 100,
              target: "host",
            },
          }}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={{}}
          ensureProviderAuthSummary={async () => undefined}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    fireEvent.click(screen.getByRole("button", { name: "Codex" }));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    const cursorRow = screen.getByText("Cursor").closest(".wb-harness-row");
    expect(cursorRow).not.toBeNull();
    expect(within(cursorRow as HTMLElement).queryByRole("button", { name: "100%" })).not.toBeInTheDocument();
    const finalizingButton = within(cursorRow as HTMLElement).getByRole("button", { name: "Finalizing…" });
    expect(finalizingButton.className).toContain("wb-harness-install-busy");
  });

  it("does not show finalizing for installed providers that still need auth", async () => {
    const onRequestHarnessAuth = vi.fn();

    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "o3" });
      const harnessCatalog: HarnessCatalogEntry[] = [
        { id: "codex", label: "Codex", logoSrc: "" },
        { id: "cursor", label: "Cursor", logoSrc: "" },
      ];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
        cursor: makeProviderStatus("cursor", {
          installed: true,
          health: "error",
          diagnostics: ["Authentication required"],
          usability: {
            usable: false,
            status: "blocked",
            blocking_provider_ids: [],
            recommended_action: "configure_runtime",
            reason: "Authentication required",
          },
          details: {
            install_supported: "true",
            install_target: "host",
          },
        }),
      };
      const providerOptions: Record<string, ProviderOptions | undefined> = {
        codex: { ...baseOptions("codex"), has_active_auth: true },
        cursor: { ...baseOptions("cursor"), has_active_auth: false },
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{
            cursor: {
              installId: "install-cursor",
              state: "succeeded",
              pct: 100,
              target: "host",
            },
          }}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={providerOptions}
          ensureProviderAuthSummary={async (providerId: string) => providerOptions[providerId]}
          onRequestHarnessAuth={onRequestHarnessAuth}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    fireEvent.click(screen.getByRole("button", { name: "Codex" }));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    const cursorRow = screen.getByText("Cursor").closest(".wb-harness-row");
    expect(cursorRow).not.toBeNull();
    expect(within(cursorRow as HTMLElement).queryByRole("button", { name: "Finalizing…" })).not.toBeInTheDocument();
    expect(within(cursorRow as HTMLElement).queryByRole("button", { name: "Install" })).not.toBeInTheDocument();
    expect(within(cursorRow as HTMLElement).getByTitle("Authentication not configured")).toBeInTheDocument();
    fireEvent.click(within(cursorRow as HTMLElement).getByRole("button", { name: /Cursor/ }));
    expect(onRequestHarnessAuth).toHaveBeenCalledWith("cursor");
  });

  it("hydrates provider auth summary even when bootstrap options already exist", async () => {
    const ensureProviderAuthSummary = vi.fn(async () => undefined);

    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "claude-crp", modelId: "" });
      const harnessCatalog: HarnessCatalogEntry[] = [{ id: "claude-crp", label: "Claude Code", logoSrc: "" }];
      const providersById: Record<string, ProviderStatus> = {
        "claude-crp": makeProviderStatus("claude-crp"),
      };
      const providerOptions: Record<string, ProviderOptions | undefined> = {
        "claude-crp": {
          ...baseOptions("claude-crp"),
          has_active_auth: true,
          auth_mode: "subscription",
          source: {
            provider_id: "claude-crp",
            selected_source_kind: "subscription",
            selected_endpoint_id: null,
            endpoints: [],
          },
        },
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={providerOptions}
          ensureProviderAuthSummary={ensureProviderAuthSummary}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="claude-crp"
        />
      );
    };

    render(<NewTaskHarness />);
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(ensureProviderAuthSummary).toHaveBeenCalledWith("claude-crp");
  });

  it("seeds the draft model from the selected endpoint override before models hydrate", async () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "" });
      const harnessCatalog: HarnessCatalogEntry[] = [{ id: "codex", label: "Codex", logoSrc: "" }];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
      };
      const providerOptions: Record<string, ProviderOptions | undefined> = {
        codex: {
          ...baseOptions("codex"),
          has_active_auth: true,
          auth_mode: "endpoint",
          source: {
            provider_id: "codex",
            selected_source_kind: "endpoint",
            selected_endpoint_id: "openrouter",
            endpoints: [
              {
                id: "openrouter",
                provider_id: "codex",
                name: "OpenRouter",
                base_url: "https://openrouter.ai/api/v1",
                api_shape: "openai_responses",
                auth_type: "bearer",
                model_override: "openai/gpt-5.2-codex",
                created_at: "2026-03-10T00:00:00.000Z",
                updated_at: "2026-03-10T00:00:00.000Z",
                last_verification_status: "valid",
                last_verification_at: null,
                last_error: null,
                has_api_key: true,
              },
            ],
          },
        },
      };

      return (
        <>
          <div data-testid="draft-model">{draftHarness?.modelId ?? ""}</div>
          <WorkbenchComposer
            variant="newSession"
            value={value}
            setValue={setValue}
            placeholder="@ for context, / for commands"
            inputDisabled={false}
            sessionIdForAutocomplete={null}
            workspaceIdForAutocomplete={null}
            slashCommands={[]}
            attachments={attachments}
            setAttachments={setAttachments}
            onSend={vi.fn()}
            sendDisabled={false}
            sendDisabledReason={null}
            onInterrupt={null}
            modeId={modeId}
            setModeId={setModeId}
            harnessCatalog={harnessCatalog}
            providersById={providersById}
            providerInstallsById={{}}
            onInstallProvider={vi.fn()}
            onInstallAllProviders={vi.fn()}
            providerOptions={providerOptions}
            ensureProviderAuthSummary={async () => providerOptions.codex}
            draftHarness={draftHarness}
            setDraftHarness={setDraftHarness}
            defaultProviderId="codex"
          />
        </>
      );
    };

    render(<NewTaskHarness />);
    await waitFor(() => {
      expect(screen.getByTestId("draft-model").textContent).toBe("openai/gpt-5.2-codex");
    });
  });

  it("does not overwrite an explicit draft model when a saved preference arrives later", async () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({
        providerId: "codex",
        modelId: "",
      });
      const [providerOptions, setProviderOptions] = useState<Record<string, ProviderOptions | undefined>>({
        codex: {
          ...baseOptions("codex"),
          has_active_auth: true,
          models: {
            current_model_id: "gpt-5.4/medium",
            models: [{ id: "gpt-5.4/medium" }, { id: "gpt-5.4/xhigh" }],
          },
        },
      });
      const harnessCatalog: HarnessCatalogEntry[] = [{ id: "codex", label: "Codex", logoSrc: "" }];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
      };

      return (
        <>
          <div data-testid="draft-model">{draftHarness?.modelId ?? ""}</div>
          <button
            type="button"
            onClick={() =>
              setDraftHarness((prev) => (prev ? { ...prev, preferenceExplicit: true } : prev))
            }
          >
            Mark Explicit
          </button>
          <button
            type="button"
            onClick={() =>
              setProviderOptions((prev) => ({
                ...prev,
                codex: prev.codex
                  ? {
                      ...prev.codex,
                      preferred_model_id: "gpt-5.4/xhigh",
                    }
                  : prev.codex,
              }))
            }
          >
            Load Saved Preference
          </button>
          <WorkbenchComposer
            variant="newSession"
            value={value}
            setValue={setValue}
            placeholder="@ for context, / for commands"
            inputDisabled={false}
            sessionIdForAutocomplete={null}
            workspaceIdForAutocomplete={null}
            slashCommands={[]}
            attachments={attachments}
            setAttachments={setAttachments}
            onSend={vi.fn()}
            sendDisabled={false}
            sendDisabledReason={null}
            onInterrupt={null}
            modeId={modeId}
            setModeId={setModeId}
            harnessCatalog={harnessCatalog}
            providersById={providersById}
            providerInstallsById={{}}
            onInstallProvider={vi.fn()}
            onInstallAllProviders={vi.fn()}
            providerOptions={providerOptions}
            ensureProviderAuthSummary={async () => providerOptions.codex}
            draftHarness={draftHarness}
            setDraftHarness={setDraftHarness}
            defaultProviderId="codex"
          />
        </>
      );
    };

    render(<NewTaskHarness />);
    await waitFor(() => {
      expect(screen.getByTestId("draft-model").textContent).toBe("gpt-5.4/medium");
    });

    fireEvent.click(screen.getByRole("button", { name: "Mark Explicit" }));
    fireEvent.click(screen.getByRole("button", { name: "Load Saved Preference" }));

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(screen.getByTestId("draft-model").textContent).toBe("gpt-5.4/medium");
  });

  it("shows an explicit unselected harness state", async () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>(null);
      const harnessCatalog: HarnessCatalogEntry[] = [{ id: "codex", label: "Codex", logoSrc: "" }];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={{}}
          ensureProviderAuthSummary={async () => undefined}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    expect(screen.getByRole("button", { name: "Select agent" })).toBeInTheDocument();
  });

  it("requests auth modal when selecting an unauthenticated harness", async () => {
    const onRequestHarnessAuth = vi.fn();

    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "" });
      const harnessCatalog: HarnessCatalogEntry[] = [
        { id: "codex", label: "Codex", logoSrc: "" },
        { id: "cursor", label: "Cursor", logoSrc: "" },
      ];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
        cursor: makeProviderStatus("cursor"),
      };
      const providerOptions: Record<string, ProviderOptions | undefined> = {
        codex: { ...baseOptions("codex"), has_active_auth: true },
        cursor: { ...baseOptions("cursor"), has_active_auth: false },
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={providerOptions}
          ensureProviderAuthSummary={async (providerId: string) => providerOptions[providerId]}
          onRequestHarnessAuth={onRequestHarnessAuth}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    fireEvent.click(screen.getByRole("button", { name: "Codex" }));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    fireEvent.click(screen.getByRole("button", { name: /Cursor/ }));
    expect(onRequestHarnessAuth).toHaveBeenCalledWith("cursor");
    expect(trackFeatureUsedMock).toHaveBeenCalledWith("harness_auth_requested", {
      provider_id: "cursor",
      entry_surface: "workbench_new_task",
    });
    expect(trackProviderSelectedMock).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Codex" })).toBeInTheDocument();
  });

  it("keeps the only auth-ready harness selected when its row is clicked again", async () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "opencode", modelId: "" });
      const harnessCatalog: HarnessCatalogEntry[] = [
        { id: "opencode", label: "OpenCode", logoSrc: "" },
        { id: "cursor", label: "Cursor", logoSrc: "" },
      ];
      const providersById: Record<string, ProviderStatus> = {
        opencode: makeProviderStatus("opencode"),
        cursor: makeProviderStatus("cursor"),
      };
      const providerOptions: Record<string, ProviderOptions | undefined> = {
        opencode: {
          ...baseOptions("opencode"),
          source: {
            provider_id: "opencode",
            selected_source_kind: "endpoint" as const,
            selected_endpoint_id: "endpoint-opencode",
            endpoints: [],
          },
        },
        cursor: { ...baseOptions("cursor"), has_active_auth: false },
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={providerOptions}
          ensureProviderAuthSummary={async (providerId: string) => providerOptions[providerId]}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="opencode"
        />
      );
    };

    render(<NewTaskHarness />);
    fireEvent.click(screen.getByRole("button", { name: "OpenCode" }));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    const openCodeButtons = screen.getAllByRole("button", { name: /OpenCode/i });
    fireEvent.click(openCodeButtons[1] as HTMLButtonElement);
    expect(screen.getByRole("button", { name: "OpenCode" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Select agent" })).not.toBeInTheDocument();
  });

  it("tracks provider_selected when the composer switches harnesses", async () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "" });
      const harnessCatalog: HarnessCatalogEntry[] = [
        { id: "codex", label: "Codex", logoSrc: "" },
        { id: "cursor", label: "Cursor", logoSrc: "" },
      ];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
        cursor: makeProviderStatus("cursor"),
      };
      const providerOptions: Record<string, ProviderOptions | undefined> = {
        codex: { ...baseOptions("codex"), has_active_auth: true },
        cursor: { ...baseOptions("cursor"), has_active_auth: true },
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={providerOptions}
          ensureProviderAuthSummary={async (providerId: string) => providerOptions[providerId]}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    fireEvent.click(screen.getByRole("button", { name: "Codex" }));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    fireEvent.click(screen.getByRole("button", { name: /Cursor/ }));
    expect(trackProviderSelectedMock).toHaveBeenCalledWith({
      providerId: "cursor",
      source: "provider_switch",
    });
  });

  it("retries model hydration only when the user opens the model menu after a failed probe", async () => {
    const ensureProviderAuthSummary = vi.fn(async () => ({
      ...baseOptions("codex"),
      has_active_auth: true,
      auth_mode: "subscription" as const,
      probe_ok: false,
      probe_error: "crp runtime closed before models.list response",
      source: {
        provider_id: "codex",
        selected_source_kind: "subscription" as const,
        selected_endpoint_id: null,
        endpoints: [],
      },
    }));

    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "" });
      const harnessCatalog: HarnessCatalogEntry[] = [{ id: "codex", label: "Codex", logoSrc: "" }];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
      };
      const providerOptions: Record<string, ProviderOptions | undefined> = {
        codex: {
          ...baseOptions("codex"),
          has_active_auth: true,
          auth_mode: "subscription",
          probe_ok: false,
          probe_error: "crp runtime closed before models.list response",
          source: {
            provider_id: "codex",
            selected_source_kind: "subscription",
            selected_endpoint_id: null,
            endpoints: [],
          },
        },
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={providerOptions}
          ensureProviderAuthSummary={ensureProviderAuthSummary}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(ensureProviderAuthSummary).toHaveBeenNthCalledWith(1, "codex");

    fireEvent.click(screen.getByRole("button", { name: "Model" }));

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(ensureProviderAuthSummary).toHaveBeenNthCalledWith(2, "codex", { trigger: "explicit" });
  });

  it("attaches pasted images in the new-task composer", async () => {
    const NewTaskHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");
      const [draftHarness, setDraftHarness] = useState<DraftHarness | null>({ providerId: "codex", modelId: "o3" });
      const harnessCatalog: HarnessCatalogEntry[] = [{ id: "codex", label: "Codex", logoSrc: "" }];
      const providersById: Record<string, ProviderStatus> = {
        codex: makeProviderStatus("codex"),
      };

      return (
        <WorkbenchComposer
          variant="newSession"
          value={value}
          setValue={setValue}
          placeholder="@ for context, / for commands"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          harnessCatalog={harnessCatalog}
          providersById={providersById}
          providerInstallsById={{}}
          onInstallProvider={vi.fn()}
          onInstallAllProviders={vi.fn()}
          providerOptions={{}}
          ensureProviderAuthSummary={async () => undefined}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId="codex"
        />
      );
    };

    render(<NewTaskHarness />);
    const textarea = screen.getByPlaceholderText("@ for context, / for commands") as HTMLTextAreaElement;
    const transfer = {
      files: [new File([Uint8Array.from([137, 80, 78, 71])], "clipboard.png", { type: "image/png" })],
      items: [],
    } as unknown as DataTransfer;

    const pasteEvent = createEvent.paste(textarea);
    Object.defineProperty(pasteEvent, "clipboardData", { value: transfer });
    fireEvent(textarea, pasteEvent);

    expect(pasteEvent.defaultPrevented).toBe(true);
    await waitFor(() => {
      expect(screen.getByAltText("clipboard.png")).toBeInTheDocument();
    });
  });

  it("attaches pasted images in the active-session composer", async () => {
    const ActiveHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");

      return (
        <WorkbenchComposer
          variant="activeSession"
          value={value}
          setValue={setValue}
          placeholder="Ask follow-ups"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          recording={false}
          harnessLabel="Codex"
          availableModels={[{ id: "o3", name: "o3" }]}
          currentModelId="o3"
          onSetModelId={vi.fn()}
        />
      );
    };

    render(<ActiveHarness />);
    const textarea = screen.getByPlaceholderText("Ask follow-ups") as HTMLTextAreaElement;
    const transfer = {
      files: [new File([Uint8Array.from([255, 216, 255, 224])], "clipboard.jpg", { type: "image/jpeg" })],
      items: [],
    } as unknown as DataTransfer;

    const pasteEvent = createEvent.paste(textarea);
    Object.defineProperty(pasteEvent, "clipboardData", { value: transfer });
    fireEvent(textarea, pasteEvent);

    expect(pasteEvent.defaultPrevented).toBe(true);
    await waitFor(() => {
      expect(screen.getByAltText("clipboard.jpg")).toBeInTheDocument();
    });
  });

  it("preserves plain text when pasted clipboard data also contains an image", async () => {
    const ActiveHarness = () => {
      const [value, setValue] = useState("before ");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");

      return (
        <WorkbenchComposer
          variant="activeSession"
          value={value}
          setValue={setValue}
          placeholder="Ask follow-ups"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          recording={false}
          harnessLabel="Codex"
          availableModels={[{ id: "o3", name: "o3" }]}
          currentModelId="o3"
          onSetModelId={vi.fn()}
        />
      );
    };

    render(<ActiveHarness />);
    const textarea = screen.getByPlaceholderText("Ask follow-ups") as HTMLTextAreaElement;
    textarea.setSelectionRange(textarea.value.length, textarea.value.length);
    const transfer = {
      files: [new File([Uint8Array.from([137, 80, 78, 71])], "clipboard.png", { type: "image/png" })],
      items: [],
      getData(type: string) {
        if (type === "text/plain") return "caption";
        return "";
      },
    } as unknown as DataTransfer;

    const pasteEvent = createEvent.paste(textarea);
    Object.defineProperty(pasteEvent, "clipboardData", { value: transfer });
    fireEvent(textarea, pasteEvent);

    expect(pasteEvent.defaultPrevented).toBe(true);
    await waitFor(() => {
      expect(textarea).toHaveValue("before caption");
      expect(screen.getByAltText("clipboard.png")).toBeInTheDocument();
    });
  });

  it("does not consume text-only paste events", () => {
    const ActiveHarness = () => {
      const [value, setValue] = useState("");
      const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
      const [modeId, setModeId] = useState<WorkbenchModeId>("default");

      return (
        <WorkbenchComposer
          variant="activeSession"
          value={value}
          setValue={setValue}
          placeholder="Ask follow-ups"
          inputDisabled={false}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={null}
          slashCommands={[]}
          attachments={attachments}
          setAttachments={setAttachments}
          onSend={vi.fn()}
          sendDisabled={false}
          sendDisabledReason={null}
          onInterrupt={null}
          modeId={modeId}
          setModeId={setModeId}
          recording={false}
          harnessLabel="Codex"
          availableModels={[{ id: "o3", name: "o3" }]}
          currentModelId="o3"
          onSetModelId={vi.fn()}
        />
      );
    };

    render(<ActiveHarness />);
    const textarea = screen.getByPlaceholderText("Ask follow-ups") as HTMLTextAreaElement;
    const transfer = {
      files: [new File(["hello"], "notes.txt", { type: "text/plain" })],
      items: [
        {
          kind: "string",
          type: "text/plain",
          getAsFile: () => null,
        },
      ],
    } as unknown as DataTransfer;

    const pasteEvent = createEvent.paste(textarea);
    Object.defineProperty(pasteEvent, "clipboardData", { value: transfer });
    fireEvent(textarea, pasteEvent);

    expect(pasteEvent.defaultPrevented).toBe(false);
    expect(document.querySelectorAll(".wb-composer-attachments .wb-attach-thumb-img")).toHaveLength(0);
  });
});
