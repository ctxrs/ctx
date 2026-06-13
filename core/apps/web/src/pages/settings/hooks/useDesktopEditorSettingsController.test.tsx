import { act, render } from "@testing-library/react";
import { useEffect } from "react";
import type { Dispatch, SetStateAction } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useDesktopEditorSettingsController } from "./useDesktopEditorSettingsController";
import {
  desktopGetEditorSettings,
  desktopUpdateEditorSettings,
  type DesktopEditorSettings,
} from "../../../utils/desktop";

vi.mock("../../../utils/desktop", async () => {
  const actual = await vi.importActual<typeof import("../../../utils/desktop")>("../../../utils/desktop");
  return {
    ...actual,
    desktopGetEditorSettings: vi.fn(),
    desktopUpdateEditorSettings: vi.fn(),
  };
});

type DesktopEditorSettingsControllerRef = {
  editorLoaded: boolean;
  editorSettings: DesktopEditorSettings;
  setEditorSettings: Dispatch<SetStateAction<DesktopEditorSettings>>;
};

function Harness({
  enabled,
  onReady,
}: {
  enabled: boolean;
  onReady: (controller: DesktopEditorSettingsControllerRef) => void;
}) {
  const controller = useDesktopEditorSettingsController(enabled);

  useEffect(() => {
    onReady(controller);
  }, [controller, onReady]);

  return null;
}

describe("useDesktopEditorSettingsController", () => {
  const desktopGetEditorSettingsMock = vi.mocked(desktopGetEditorSettings);
  const desktopUpdateEditorSettingsMock = vi.mocked(desktopUpdateEditorSettings);

  beforeEach(() => {
    vi.useFakeTimers();
    desktopGetEditorSettingsMock.mockReset();
    desktopUpdateEditorSettingsMock.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("does not re-save identical server responses", async () => {
    desktopGetEditorSettingsMock.mockResolvedValue({
      target: "system",
      custom_command: null,
      remote_authority: null,
    });
    desktopUpdateEditorSettingsMock.mockResolvedValue({
      target: "cursor",
      custom_command: null,
      remote_authority: null,
    });

    const controllerRef: { current: DesktopEditorSettingsControllerRef | null } = { current: null };
    render(<Harness enabled onReady={(controller) => {
      controllerRef.current = controller;
    }} />);

    await act(async () => {
      await Promise.resolve();
    });
    expect(controllerRef.current?.editorLoaded).toBe(true);

    act(() => {
      controllerRef.current?.setEditorSettings((prev) => ({
        ...prev,
        target: "cursor",
      }));
    });

    await act(async () => {
      vi.advanceTimersByTime(350);
      await Promise.resolve();
    });

    expect(desktopUpdateEditorSettingsMock).toHaveBeenCalledTimes(1);
    expect(desktopUpdateEditorSettingsMock).toHaveBeenCalledWith({
      target: "cursor",
      custom_command: null,
      remote_authority: null,
    });

    await act(async () => {
      await Promise.resolve();
      vi.advanceTimersByTime(1000);
      await Promise.resolve();
    });

    expect(desktopUpdateEditorSettingsMock).toHaveBeenCalledTimes(1);
  });

  it("does not clobber newer local edits with an older save response", async () => {
    desktopGetEditorSettingsMock.mockResolvedValue({
      target: "cursor",
      custom_command: null,
      remote_authority: "ssh-remote+old",
    });

    let resolveSave: ((value: {
      target: "cursor";
      custom_command: string | null;
      remote_authority: string | null;
    }) => void) | null = null;
    desktopUpdateEditorSettingsMock.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveSave = resolve;
        }),
    );

    const controllerRef: { current: DesktopEditorSettingsControllerRef | null } = { current: null };
    render(<Harness enabled onReady={(controller) => {
      controllerRef.current = controller;
    }} />);

    await act(async () => {
      await Promise.resolve();
    });
    expect(controllerRef.current?.editorLoaded).toBe(true);

    act(() => {
      controllerRef.current?.setEditorSettings((prev) => ({
        ...prev,
        remote_authority: "ssh-remote+first",
      }));
    });

    await act(async () => {
      vi.advanceTimersByTime(350);
      await Promise.resolve();
    });

    act(() => {
      controllerRef.current?.setEditorSettings((prev) => ({
        ...prev,
        remote_authority: "ssh-remote+second",
      }));
    });

    await act(async () => {
      resolveSave?.({
        target: "cursor",
        custom_command: null,
        remote_authority: "ssh-remote+first",
      });
      await Promise.resolve();
    });

    expect(controllerRef.current?.editorSettings.remote_authority).toBe("ssh-remote+second");
  });

  it("normalizes legacy custom settings loaded from desktop storage", async () => {
    desktopGetEditorSettingsMock.mockResolvedValue({
      target: "custom",
      custom_command: "code --goto {path}:{line}:{col}",
      remote_authority: " ssh-remote+ctx ",
    });
    desktopUpdateEditorSettingsMock.mockResolvedValue({
      target: "system",
      custom_command: null,
      remote_authority: "ssh-remote+ctx",
    });

    const controllerRef: { current: DesktopEditorSettingsControllerRef | null } = { current: null };
    render(<Harness enabled onReady={(controller) => {
      controllerRef.current = controller;
    }} />);

    await act(async () => {
      await Promise.resolve();
    });

    expect(controllerRef.current?.editorSettings).toEqual({
      target: "system",
      custom_command: null,
      remote_authority: "ssh-remote+ctx",
    });
    expect(desktopUpdateEditorSettingsMock).not.toHaveBeenCalled();
  });
});
