import { useEffect, useRef, useState } from "react";
import {
  desktopGetUpdateChannel,
  desktopUpdateUpdateChannel,
  type DesktopUpdateChannelSettings,
} from "../../../utils/desktop";
import { errorMessage } from "../../../utils/errorMessage";

type DesktopUpdateChannelController = {
  updateChannel: DesktopUpdateChannelSettings["channel"];
  setUpdateChannel: (channel: DesktopUpdateChannelSettings["channel"]) => void;
  updateChannelLoaded: boolean;
  updateChannelSaving: boolean;
  updateChannelError: string | null;
};

const DEFAULT_UPDATE_CHANNEL = "stable";

const normalizeUpdateChannel = (raw: string | null | undefined): DesktopUpdateChannelSettings["channel"] => {
  const channel = String(raw || "").trim().toLowerCase();
  return channel === "canary" ? "canary" : DEFAULT_UPDATE_CHANNEL;
};

export function useDesktopUpdateChannelController(enabled: boolean): DesktopUpdateChannelController {
  const [updateChannel, setUpdateChannelState] =
    useState<DesktopUpdateChannelSettings["channel"]>(DEFAULT_UPDATE_CHANNEL);
  const [updateChannelLoaded, setUpdateChannelLoaded] = useState(false);
  const [updateChannelSaving, setUpdateChannelSaving] = useState(false);
  const [updateChannelError, setUpdateChannelError] = useState<string | null>(null);
  const hydrated = useRef(false);
  const lastSavedChannelRef = useRef<DesktopUpdateChannelSettings["channel"] | null>(null);

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    desktopGetUpdateChannel()
      .then((settings) => {
        if (cancelled) return;
        const normalized = normalizeUpdateChannel(settings.channel);
        setUpdateChannelState(normalized);
        lastSavedChannelRef.current = normalized;
        setUpdateChannelLoaded(true);
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        setUpdateChannelError(errorMessage(error));
        setUpdateChannelLoaded(true);
      });
    return () => {
      cancelled = true;
    };
  }, [enabled]);

  useEffect(() => {
    if (!enabled || !updateChannelLoaded) return;
    if (!hydrated.current) {
      hydrated.current = true;
      if (!lastSavedChannelRef.current) {
        lastSavedChannelRef.current = updateChannel;
      }
      return;
    }
    if (updateChannel === lastSavedChannelRef.current) return;

    const pendingChannel = normalizeUpdateChannel(updateChannel);
    const timeout = window.setTimeout(() => {
      setUpdateChannelSaving(true);
      setUpdateChannelError(null);
      desktopUpdateUpdateChannel({ channel: pendingChannel })
        .then((next) => {
          const normalized = normalizeUpdateChannel(next.channel);
          lastSavedChannelRef.current = normalized;
          setUpdateChannelState((current) => (current === pendingChannel ? normalized : current));
        })
        .catch((error: unknown) => {
          setUpdateChannelError(errorMessage(error));
        })
        .finally(() => {
          setUpdateChannelSaving(false);
        });
    }, 350);

    return () => {
      window.clearTimeout(timeout);
    };
  }, [enabled, updateChannel, updateChannelLoaded]);

  return {
    updateChannel,
    setUpdateChannel: (channel) => setUpdateChannelState(normalizeUpdateChannel(channel)),
    updateChannelLoaded,
    updateChannelSaving,
    updateChannelError,
  };
}

export type { DesktopUpdateChannelController };
