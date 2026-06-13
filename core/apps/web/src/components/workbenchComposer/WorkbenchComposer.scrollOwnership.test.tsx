import { act, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WorkbenchComposer } from "../WorkbenchComposer";
import type { MessageAttachment } from "../../api/client";
import type { WorkbenchModeId } from "../WorkbenchComposer";

function mockRaf() {
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((cb: FrameRequestCallback) => {
    return window.setTimeout(() => cb(performance.now()), 0) as unknown as number;
  });
}

function defineScrollMetrics(
  element: HTMLElement,
  {
    clientHeight,
    scrollHeight,
    scrollTop,
  }: {
    clientHeight: number;
    scrollHeight: number;
    scrollTop: number;
  },
) {
  let scrollTopValue = scrollTop;

  Object.defineProperty(element, "clientHeight", {
    configurable: true,
    get: () => clientHeight,
  });
  Object.defineProperty(element, "scrollHeight", {
    configurable: true,
    get: () => scrollHeight,
  });
  Object.defineProperty(element, "scrollTop", {
    configurable: true,
    get: () => scrollTopValue,
    set: (value: number) => {
      scrollTopValue = value;
    },
  });
}

function ActiveComposerHarness({ initialValue }: { initialValue: string }) {
  const [value, setValue] = useState(initialValue);
  const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
  const [modeId, setModeId] = useState<WorkbenchModeId>("default");

  return (
    <div className="wb-session-view">
      <div className="wb-thread-scroller" data-testid="thread-scroller" />
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
        isWorking={false}
        modeId={modeId}
        setModeId={setModeId}
        recording={false}
        harnessLabel="Codex"
        harnessLogoSrc=""
        harnessLogoInvert={false}
        harnessLogoInvertInLight={false}
        verbosity="default"
        onSetVerbosity={undefined}
        contextWindow={null}
        availableModels={[{ id: "gpt-5.4", name: "gpt-5.4" }]}
        currentModelId="gpt-5.4"
        onSetModelId={vi.fn(async () => {})}
      />
    </div>
  );
}

describe("WorkbenchComposer wheel ownership", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    mockRaf();
  });

  afterEach(() => {
    vi.runOnlyPendingTimers();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("hands wheel-up input to the transcript as soon as the composer reaches its top edge", async () => {
    render(<ActiveComposerHarness initialValue={Array.from({ length: 40 }, (_, i) => `line ${i + 1}`).join("\n")} />);
    const textarea = screen.getByPlaceholderText("Ask follow-ups") as HTMLTextAreaElement;
    const scroller = screen.getByTestId("thread-scroller") as HTMLDivElement;
    textarea.style.lineHeight = "20px";

    defineScrollMetrics(textarea, {
      clientHeight: 120,
      scrollHeight: 360,
      scrollTop: 20,
    });
    defineScrollMetrics(scroller, {
      clientHeight: 480,
      scrollHeight: 1600,
      scrollTop: 600,
    });

    await act(async () => {
      fireEvent.wheel(textarea, { deltaY: -40, deltaMode: 0 });
    });
    expect(textarea.scrollTop).toBe(0);
    expect(scroller.scrollTop).toBe(600);

    await act(async () => {
      fireEvent.wheel(textarea, { deltaY: -40, deltaMode: 0 });
    });
    expect(textarea.scrollTop).toBe(0);
    expect(scroller.scrollTop).toBe(560);
  });

  it("hands wheel-down input to the transcript as soon as the composer reaches its bottom edge", async () => {
    render(<ActiveComposerHarness initialValue={Array.from({ length: 40 }, (_, i) => `line ${i + 1}`).join("\n")} />);
    const textarea = screen.getByPlaceholderText("Ask follow-ups") as HTMLTextAreaElement;
    const scroller = screen.getByTestId("thread-scroller") as HTMLDivElement;
    textarea.style.lineHeight = "20px";

    defineScrollMetrics(textarea, {
      clientHeight: 120,
      scrollHeight: 360,
      scrollTop: 220,
    });
    defineScrollMetrics(scroller, {
      clientHeight: 480,
      scrollHeight: 1600,
      scrollTop: 600,
    });

    await act(async () => {
      fireEvent.wheel(textarea, { deltaY: 40, deltaMode: 0 });
    });
    expect(textarea.scrollTop).toBe(240);
    expect(scroller.scrollTop).toBe(600);

    await act(async () => {
      fireEvent.wheel(textarea, { deltaY: 40, deltaMode: 0 });
    });
    expect(textarea.scrollTop).toBe(240);
    expect(scroller.scrollTop).toBe(640);
  });

  it("hands the burst to the transcript immediately when the composer starts at the edge", async () => {
    render(<ActiveComposerHarness initialValue={Array.from({ length: 40 }, (_, i) => `line ${i + 1}`).join("\n")} />);
    const textarea = screen.getByPlaceholderText("Ask follow-ups") as HTMLTextAreaElement;
    const scroller = screen.getByTestId("thread-scroller") as HTMLDivElement;
    textarea.style.lineHeight = "20px";

    defineScrollMetrics(textarea, {
      clientHeight: 120,
      scrollHeight: 360,
      scrollTop: 0,
    });
    defineScrollMetrics(scroller, {
      clientHeight: 480,
      scrollHeight: 1600,
      scrollTop: 600,
    });

    await act(async () => {
      fireEvent.wheel(textarea, { deltaY: -40, deltaMode: 0 });
    });
    expect(textarea.scrollTop).toBe(0);
    expect(scroller.scrollTop).toBe(560);

    await act(async () => {
      fireEvent.wheel(textarea, { deltaY: -40, deltaMode: 0 });
    });
    expect(textarea.scrollTop).toBe(0);
    expect(scroller.scrollTop).toBe(520);
  });
});
