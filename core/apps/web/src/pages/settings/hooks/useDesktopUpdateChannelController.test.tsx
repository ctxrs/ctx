import { act, render } from "@testing-library/react";
import { useEffect } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useDesktopUpdateChannelController } from "./useDesktopUpdateChannelController";
import {
  desktopGetUpdateChannel,
  desktopUpdateUpdateChannel,
  type DesktopUpdateChannelSettings,
} from "../../../utils/desktop";

vi.mock("../../../utils/desktop", async () => {
  const actual = await vi.importActual<typeof import("../../../utils/desktop")>("../../../utils/desktop");
  return {
    ...actual,
    desktopGetUpdateChannel: vi.fn(),
    desktopUpdateUpdateChannel: vi.fn(),
  };
});

type DesktopUpdateChannelControllerRef = {
  updateChannel: DesktopUpdateChannelSettings["channel"];
  setUpdateChannel: (channel: DesktopUpdateChannelSettings["channel"]) => void;
  updateChannelLoaded: boolean;
  updateChannelError: string | null;
};

function Harness({
  enabled,
  onReady,
}: {
  enabled: boolean;
  onReady: (controller: DesktopUpdateChannelControllerRef) => void;
}) {
  const controller = useDesktopUpdateChannelController(enabled);

  useEffect(() => {
    onReady(controller);
  }, [controller, onReady]);

  return null;
}

describe("useDesktopUpdateChannelController", () => {
  const desktopGetUpdateChannelMock = vi.mocked(desktopGetUpdateChannel);
  const desktopUpdateUpdateChannelMock = vi.mocked(desktopUpdateUpdateChannel);

  beforeEach(() => {
    vi.useFakeTimers();
    desktopGetUpdateChannelMock.mockReset();
    desktopUpdateUpdateChannelMock.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("loads stable by default without writing back", async () => {
    desktopGetUpdateChannelMock.mockResolvedValue({ channel: "stable" });
    desktopUpdateUpdateChannelMock.mockResolvedValue({ channel: "stable" });

    const controllerRef: { current: DesktopUpdateChannelControllerRef | null } = { current: null };
    render(<Harness enabled onReady={(controller) => {
      controllerRef.current = controller;
    }} />);

    await act(async () => {
      await Promise.resolve();
    });

    expect(controllerRef.current?.updateChannelLoaded).toBe(true);
    expect(controllerRef.current?.updateChannel).toBe("stable");
    expect(desktopUpdateUpdateChannelMock).not.toHaveBeenCalled();
  });

  it("persists canary opt-in after load", async () => {
    desktopGetUpdateChannelMock.mockResolvedValue({ channel: "stable" });
    desktopUpdateUpdateChannelMock.mockResolvedValue({ channel: "canary" });

    const controllerRef: { current: DesktopUpdateChannelControllerRef | null } = { current: null };
    render(<Harness enabled onReady={(controller) => {
      controllerRef.current = controller;
    }} />);

    await act(async () => {
      await Promise.resolve();
    });

    act(() => {
      controllerRef.current?.setUpdateChannel("canary");
    });

    await act(async () => {
      vi.advanceTimersByTime(350);
      await Promise.resolve();
    });

    expect(desktopUpdateUpdateChannelMock).toHaveBeenCalledTimes(1);
    expect(desktopUpdateUpdateChannelMock).toHaveBeenCalledWith({ channel: "canary" });
    expect(controllerRef.current?.updateChannel).toBe("canary");
  });

  it("normalizes unknown desktop responses back to stable", async () => {
    desktopGetUpdateChannelMock.mockResolvedValue({ channel: "nightly" });
    desktopUpdateUpdateChannelMock.mockResolvedValue({ channel: "stable" });

    const controllerRef: { current: DesktopUpdateChannelControllerRef | null } = { current: null };
    render(<Harness enabled onReady={(controller) => {
      controllerRef.current = controller;
    }} />);

    await act(async () => {
      await Promise.resolve();
    });

    expect(controllerRef.current?.updateChannel).toBe("stable");
    expect(desktopUpdateUpdateChannelMock).not.toHaveBeenCalled();
  });

  it("surfaces load errors and still defaults to stable", async () => {
    desktopGetUpdateChannelMock.mockRejectedValue(new Error("settings unreadable"));

    const controllerRef: { current: DesktopUpdateChannelControllerRef | null } = { current: null };
    render(<Harness enabled onReady={(controller) => {
      controllerRef.current = controller;
    }} />);

    await act(async () => {
      await Promise.resolve();
    });

    expect(controllerRef.current?.updateChannelLoaded).toBe(true);
    expect(controllerRef.current?.updateChannel).toBe("stable");
    expect(controllerRef.current?.updateChannelError).toBe("settings unreadable");
  });
});
