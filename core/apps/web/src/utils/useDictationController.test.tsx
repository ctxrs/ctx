import { act, render, screen, waitFor } from "@testing-library/react";
import { useEffect, useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { DictationSettings, PublicSettings } from "../api/client";
import { appendSegment } from "../pages/SessionPage.helpers";
import type { SttApi, SttResult } from "./tauriStt";
import { useDictationController } from "./useDictationController";
import { getSettings, updateSettings } from "../api/client";
import { trackFeatureUsed } from "./analytics";
import { loadSttApi } from "./tauriStt";

const deferred = <T,>() => {
  let resolve: (value: T | PromiseLike<T>) => void = () => {};
  let reject: (reason?: unknown) => void = () => {};
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
};

vi.mock("../api/client", async () => {
  const actual = await vi.importActual<typeof import("../api/client")>("../api/client");
  return {
    ...actual,
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
  };
});

vi.mock("./desktop", () => ({
  isDesktopApp: () => true,
}));

vi.mock("./tauriStt", async () => {
  const actual = await vi.importActual<typeof import("./tauriStt")>("./tauriStt");
  return {
    ...actual,
    loadSttApi: vi.fn(),
  };
});

vi.mock("./analytics", async () => {
  const actual = await vi.importActual<typeof import("./analytics")>("./analytics");
  return {
    ...actual,
    trackFeatureUsed: vi.fn(),
  };
});

function Harness({ onReady }: { onReady: (controller: ReturnType<typeof useDictationController>) => void }) {
  const [text, setText] = useState("");
  const controller = useDictationController({ text, setText, appendSegment });

  useEffect(() => {
    onReady(controller);
  }, [controller, onReady]);

  return (
    <div>
      <div data-testid="text">{text}</div>
      <div data-testid="recording">{controller.dictationRecording ? "on" : "off"}</div>
    </div>
  );
}

describe("useDictationController integration", () => {
  const getSettingsMock = vi.mocked(getSettings);
  const updateSettingsMock = vi.mocked(updateSettings);
  const loadSttApiMock = vi.mocked(loadSttApi);
  const trackFeatureUsedMock = vi.mocked(trackFeatureUsed);

  beforeEach(() => {
    getSettingsMock.mockReset();
    updateSettingsMock.mockReset();
    loadSttApiMock.mockReset();
    trackFeatureUsedMock.mockReset();
  });

  it("streams Tauri dictation interim/final text and stops cleanly", async () => {
    const settings: PublicSettings = {
      dictation: {
        enabled: true,
        provider: "tauri_stt",
        livekit: {
          base_url: "",
          api_key_set: false,
          api_secret_set: false,
          model: "auto",
          language: "en",
        },
      },
    };
    getSettingsMock.mockResolvedValue(settings);

    let resultHandler: ((result: SttResult) => void) | null = null;

    const sttApi: SttApi = {
      isAvailable: vi.fn(async () => ({ available: true })),
      getSupportedLanguages: vi.fn(async () => ({ languages: [] })),
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
      onResult: vi.fn(async (handler) => {
        resultHandler = handler;
        return () => {};
      }),
      onStateChange: vi.fn(async () => () => {}),
      onError: vi.fn(async () => () => {}),
    };
    loadSttApiMock.mockResolvedValue(sttApi);

    let controllerRef: ReturnType<typeof useDictationController> | null = null;
    render(<Harness onReady={(controller) => (controllerRef = controller)} />);

    await waitFor(() => expect(controllerRef).not.toBeNull());

    await act(async () => {
      await controllerRef!.startDictation();
    });

    await waitFor(() => expect(screen.getByTestId("recording").textContent).toBe("on"));
    expect(trackFeatureUsedMock).toHaveBeenCalledWith("dictation_started", {
      provider: "tauri_stt",
      path: "configured",
    });

    await act(async () => {
      resultHandler?.({ transcript: "hello", isFinal: false });
    });
    expect(screen.getByTestId("text").textContent).toBe("hello");

    await act(async () => {
      resultHandler?.({ transcript: "hello world", isFinal: true });
    });
    expect(screen.getByTestId("text").textContent).toBe("hello world");

    await act(async () => {
      await controllerRef!.stopDictation();
    });
    await waitFor(() => expect(screen.getByTestId("recording").textContent).toBe("off"));
    expect(sttApi.stopListening).toHaveBeenCalled();
  });

  it("opens onboarding modal when dictation settings are missing", async () => {
    getSettingsMock.mockResolvedValue({});

    let controllerRef: ReturnType<typeof useDictationController> | null = null;
    render(<Harness onReady={(controller) => (controllerRef = controller)} />);
    await waitFor(() => expect(controllerRef).not.toBeNull());

    await act(async () => {
      await controllerRef!.startDictation();
    });

    await waitFor(() => {
      expect(controllerRef!.dictationOnboarding?.open).toBe(true);
      expect(controllerRef!.dictationOnboarding?.stage).toBe("choose");
    });
    expect(controllerRef!.dictationError).toBeNull();
    expect(trackFeatureUsedMock).toHaveBeenCalledWith("dictation_started", {
      provider: "unknown",
      path: "onboarding_required",
    });
  });

  it("opens onboarding modal when dictation is disabled", async () => {
    const settings: PublicSettings = {
      dictation: {
        enabled: false,
        provider: "disabled",
        livekit: null,
      } satisfies DictationSettings,
    };
    getSettingsMock.mockResolvedValue(settings);

    let controllerRef: ReturnType<typeof useDictationController> | null = null;
    render(<Harness onReady={(controller) => (controllerRef = controller)} />);
    await waitFor(() => expect(controllerRef).not.toBeNull());

    await act(async () => {
      await controllerRef!.startDictation();
    });

    await waitFor(() => {
      expect(controllerRef!.dictationOnboarding?.open).toBe(true);
      expect(controllerRef!.dictationOnboarding?.stage).toBe("choose");
    });
  });

  it("saves onboarding cloud settings and auto-starts dictation", async () => {
    getSettingsMock.mockResolvedValue({});

    const sttApi: SttApi = {
      isAvailable: vi.fn(async () => ({ available: true })),
      getSupportedLanguages: vi.fn(async () => ({ languages: [] })),
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
    updateSettingsMock.mockResolvedValue({
      dictation: {
        enabled: true,
        provider: "tauri_stt",
        livekit: {
          base_url: "",
          api_key_set: false,
          api_secret_set: false,
          model: "auto",
          language: "en",
        },
      },
    });

    let controllerRef: ReturnType<typeof useDictationController> | null = null;
    render(<Harness onReady={(controller) => (controllerRef = controller)} />);
    await waitFor(() => expect(controllerRef).not.toBeNull());

    await act(async () => {
      await controllerRef!.startDictation();
    });
    await waitFor(() => expect(controllerRef!.dictationOnboarding?.open).toBe(true));

    act(() => {
      controllerRef!.chooseDictationOnboardingCloud();
      controllerRef!.updateDictationOnboardingCloud({
        apiKey: "lk-key",
        apiSecret: "lk-secret",
      });
    });

    await act(async () => {
      await controllerRef!.submitDictationOnboardingCloud();
    });

    expect(updateSettingsMock).toHaveBeenCalledWith(
      expect.objectContaining({
        dictation: expect.objectContaining({
          enabled: true,
          provider: "livekit_inference",
        }),
      }),
    );
    await waitFor(() => expect(screen.getByTestId("recording").textContent).toBe("on"));
    expect(controllerRef!.dictationOnboarding).toBeNull();
  });

  it("preserves stored cloud credentials when onboarding submits blank redacted fields", async () => {
    getSettingsMock.mockResolvedValue({
      dictation: {
        enabled: false,
        provider: "disabled",
        livekit: {
          base_url: "https://livekit.example",
          api_key_set: true,
          api_secret_set: true,
          model: "auto",
          language: "en",
        },
      },
    });

    const sttApi: SttApi = {
      isAvailable: vi.fn(async () => ({ available: true })),
      getSupportedLanguages: vi.fn(async () => ({ languages: [] })),
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
    updateSettingsMock.mockResolvedValue({
      dictation: {
        enabled: true,
        provider: "tauri_stt",
        livekit: {
          base_url: "https://livekit.example",
          api_key_set: true,
          api_secret_set: true,
          model: "auto",
          language: "en",
        },
      },
    });

    let controllerRef: ReturnType<typeof useDictationController> | null = null;
    render(<Harness onReady={(controller) => (controllerRef = controller)} />);
    await waitFor(() => expect(controllerRef).not.toBeNull());

    await act(async () => {
      await controllerRef!.startDictation();
    });
    await waitFor(() => expect(controllerRef!.dictationOnboarding?.open).toBe(true));

    act(() => {
      controllerRef!.chooseDictationOnboardingCloud();
    });

    await act(async () => {
      await controllerRef!.submitDictationOnboardingCloud();
    });

    expect(updateSettingsMock).toHaveBeenCalledWith({
      dictation: {
        enabled: true,
        provider: "livekit_inference",
        livekit: {
          base_url: "https://livekit.example",
          model: "auto",
          language: "en",
        },
      },
    });
    await waitFor(() => expect(screen.getByTestId("recording").textContent).toBe("on"));
    expect(controllerRef!.dictationOnboarding).toBeNull();
  });

  it("does not persist local provider when local start fails", async () => {
    getSettingsMock.mockResolvedValue({});

    const sttApi: SttApi = {
      isAvailable: vi.fn(async () => ({ available: true })),
      getSupportedLanguages: vi.fn(async () => ({ languages: [{ code: "en-US", installed: true, name: "English" }] })),
      checkPermission: vi.fn(async () => ({
        microphone: "denied",
        speechRecognition: "denied",
      } as const)),
      requestPermission: vi.fn(async () => ({
        microphone: "denied",
        speechRecognition: "denied",
      } as const)),
      startListening: vi.fn(async () => {}),
      stopListening: vi.fn(async () => {}),
      onResult: vi.fn(async () => () => {}),
      onStateChange: vi.fn(async () => () => {}),
      onError: vi.fn(async () => () => {}),
    };
    loadSttApiMock.mockResolvedValue(sttApi);

    let controllerRef: ReturnType<typeof useDictationController> | null = null;
    render(<Harness onReady={(controller) => (controllerRef = controller)} />);
    await waitFor(() => expect(controllerRef).not.toBeNull());

    await act(async () => {
      await controllerRef!.startDictation();
    });
    await waitFor(() => expect(controllerRef!.dictationOnboarding?.open).toBe(true));

    act(() => {
      controllerRef!.chooseDictationOnboardingLocal();
    });

    await waitFor(() => expect(controllerRef!.dictationOnboarding?.stage).toBe("local_setup"));
    await waitFor(() => {
      const status = controllerRef!.dictationOnboarding?.localModelStatus.status;
      expect(status === "ready" || controllerRef!.dictationOnboarding?.localModelStatus.installed).toBe(true);
    });

    await act(async () => {
      await controllerRef!.submitDictationOnboardingLocal();
    });

    expect(updateSettingsMock).not.toHaveBeenCalled();
    expect(controllerRef!.dictationOnboarding?.open).toBe(true);
    expect(controllerRef!.dictationOnboarding?.error).toBe("Failed to start local dictation.");
    expect(sttApi.startListening).not.toHaveBeenCalled();
  });

  it("cancels in-flight onboarding submit when modal closes", async () => {
    getSettingsMock.mockResolvedValue({});

    const sttApi: SttApi = {
      isAvailable: vi.fn(async () => ({ available: true })),
      getSupportedLanguages: vi.fn(async () => ({ languages: [{ code: "en-US", installed: true, name: "English" }] })),
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

    const updateDeferred = deferred<PublicSettings>();
    updateSettingsMock.mockReturnValue(updateDeferred.promise);

    let controllerRef: ReturnType<typeof useDictationController> | null = null;
    render(<Harness onReady={(controller) => (controllerRef = controller)} />);
    await waitFor(() => expect(controllerRef).not.toBeNull());

    await act(async () => {
      await controllerRef!.startDictation();
    });
    await waitFor(() => expect(controllerRef!.dictationOnboarding?.open).toBe(true));

    act(() => {
      controllerRef!.chooseDictationOnboardingCloud();
      controllerRef!.updateDictationOnboardingCloud({
        apiKey: "lk-key",
        apiSecret: "lk-secret",
      });
    });

    let submitPromise: Promise<void> = Promise.resolve();
    await act(async () => {
      submitPromise = controllerRef!.submitDictationOnboardingCloud();
      await Promise.resolve();
    });

    act(() => {
      controllerRef!.dismissDictationOnboarding();
    });

    await act(async () => {
      updateDeferred.resolve({
        dictation: {
          enabled: true,
          provider: "tauri_stt",
          livekit: {
            base_url: "",
            api_key_set: false,
            api_secret_set: false,
            model: "auto",
            language: "en",
          },
        },
      });
      await submitPromise;
    });

    expect(updateSettingsMock).toHaveBeenCalledTimes(1);
    expect(sttApi.startListening).not.toHaveBeenCalled();
    expect(controllerRef!.dictationOnboarding).toBeNull();
    expect(screen.getByTestId("recording").textContent).toBe("off");
  });

  it("clears stale dictation error after onboarding retry succeeds", async () => {
    getSettingsMock.mockResolvedValue({});

    const sttApi: SttApi = {
      isAvailable: vi.fn(async () => ({ available: true })),
      getSupportedLanguages: vi.fn(async () => ({ languages: [] })),
      checkPermission: vi
        .fn()
        .mockResolvedValueOnce({
          microphone: "denied",
          speechRecognition: "denied",
        } as const)
        .mockResolvedValue({
          microphone: "granted",
          speechRecognition: "granted",
        } as const),
      requestPermission: vi.fn(async () => ({
        microphone: "denied",
        speechRecognition: "denied",
      } as const)),
      startListening: vi.fn(async () => {}),
      stopListening: vi.fn(async () => {}),
      onResult: vi.fn(async () => () => {}),
      onStateChange: vi.fn(async () => () => {}),
      onError: vi.fn(async () => () => {}),
    };
    loadSttApiMock.mockResolvedValue(sttApi);
    updateSettingsMock.mockResolvedValue({
      dictation: {
        enabled: true,
        provider: "tauri_stt",
        livekit: {
          base_url: "",
          api_key_set: false,
          api_secret_set: false,
          model: "auto",
          language: "en",
        },
      },
    });

    let controllerRef: ReturnType<typeof useDictationController> | null = null;
    render(<Harness onReady={(controller) => (controllerRef = controller)} />);
    await waitFor(() => expect(controllerRef).not.toBeNull());

    await act(async () => {
      await controllerRef!.startDictation();
    });
    await waitFor(() => expect(controllerRef!.dictationOnboarding?.open).toBe(true));

    act(() => {
      controllerRef!.chooseDictationOnboardingCloud();
      controllerRef!.updateDictationOnboardingCloud({
        apiKey: "lk-key",
        apiSecret: "lk-secret",
      });
    });

    await act(async () => {
      await controllerRef!.submitDictationOnboardingCloud();
    });
    expect(controllerRef!.dictationError).toBe("Microphone or speech recognition permission denied.");
    expect(screen.getByTestId("recording").textContent).toBe("off");

    await act(async () => {
      await controllerRef!.submitDictationOnboardingCloud();
    });
    await waitFor(() => expect(screen.getByTestId("recording").textContent).toBe("on"));
    expect(controllerRef!.dictationError).toBeNull();
  });

  it("blocks local onboarding submit while model status is still checking", async () => {
    getSettingsMock.mockResolvedValue({});
    loadSttApiMock.mockImplementation(() => new Promise<SttApi | null>(() => {}));

    let controllerRef: ReturnType<typeof useDictationController> | null = null;
    render(<Harness onReady={(controller) => (controllerRef = controller)} />);
    await waitFor(() => expect(controllerRef).not.toBeNull());

    await act(async () => {
      await controllerRef!.startDictation();
    });
    await waitFor(() => expect(controllerRef!.dictationOnboarding?.open).toBe(true));

    act(() => {
      controllerRef!.chooseDictationOnboardingLocal();
    });

    await waitFor(() => expect(controllerRef!.dictationOnboarding?.stage).toBe("local_setup"));
    await waitFor(() => expect(controllerRef!.dictationOnboarding?.localModelStatus.status).toBe("checking"));

    await act(async () => {
      await controllerRef!.submitDictationOnboardingLocal();
    });

    expect(controllerRef!.dictationOnboarding?.error).toBe("Checking local model status. Please wait.");
    expect(controllerRef!.dictationOnboarding?.busy).toBe(false);
    expect(controllerRef!.dictationOnboarding?.localPendingStart).toBe(false);
    expect(updateSettingsMock).not.toHaveBeenCalled();
    expect(screen.getByTestId("recording").textContent).toBe("off");
  });
});
