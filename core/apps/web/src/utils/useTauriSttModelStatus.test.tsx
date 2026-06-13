import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import type { SttApi } from "./tauriStt";
import { useTauriSttModelStatus } from "./useTauriSttModelStatus";
import { desktopListen } from "./desktop";
import { loadSttApi } from "./tauriStt";

vi.mock("./desktop", () => ({
  isDesktopApp: () => true,
  desktopListen: vi.fn(),
}));

vi.mock("./tauriStt", async () => {
  const actual = await vi.importActual<typeof import("./tauriStt")>("./tauriStt");
  return {
    ...actual,
    loadSttApi: vi.fn(),
  };
});

function Harness({ provider, language }: { provider: "tauri_stt" | "livekit_inference"; language: string }) {
  const { modelStatus, startModelDownload } = useTauriSttModelStatus({ provider, language });
  return (
    <div>
      <div data-testid="status">{modelStatus.status}</div>
      <div data-testid="progress">{modelStatus.progress ?? ""}</div>
      <button type="button" onClick={() => startModelDownload().catch(() => {})}>
        Download
      </button>
    </div>
  );
}

describe("useTauriSttModelStatus integration", () => {
  const loadSttApiMock = vi.mocked(loadSttApi);
  const desktopListenMock = vi.mocked(desktopListen);

  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it("downloads a missing model and marks it ready", async () => {
    const flushPromises = async () => {
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    };
    const languages = [{ code: "en-US", name: "English (US)", installed: false }];
    let downloadHandler: ((payload: { progress?: number; status?: string }) => void) | null = null;

    const sttApi: SttApi = {
      isAvailable: vi.fn(async () => ({ available: true })),
      getSupportedLanguages: vi.fn(async () => ({ languages })),
      checkPermission: vi.fn(async () => ({
        microphone: "granted",
        speechRecognition: "granted",
      } as const)),
      requestPermission: vi.fn(async () => ({
        microphone: "granted",
        speechRecognition: "granted",
      } as const)),
      startListening: vi.fn(async () => {}),
      stopListening: vi.fn(async () => {}),
      onResult: vi.fn(async () => () => {}),
      onStateChange: vi.fn(async () => () => {}),
      onError: vi.fn(async () => () => {}),
    };

    loadSttApiMock.mockResolvedValue(sttApi);
    desktopListenMock.mockImplementation(async (_event, handler) => {
      downloadHandler = handler;
      return () => {};
    });

    render(<Harness provider="tauri_stt" language="en" />);

    await act(async () => {
      await flushPromises();
    });
    expect(screen.getByTestId("status").textContent).toBe("missing");

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Download" }));
    });

    await act(async () => {
      await flushPromises();
    });
    expect(sttApi.startListening).toHaveBeenCalled();

    act(() => {
      downloadHandler?.({ progress: 25, status: "downloading" });
    });
    expect(screen.getByTestId("status").textContent).toBe("downloading");

    languages[0].installed = true;
    await act(async () => {
      vi.advanceTimersByTime(2000);
      await flushPromises();
    });
    expect(screen.getByTestId("status").textContent).toBe("ready");
    expect(sttApi.stopListening).toHaveBeenCalled();
  });
});
