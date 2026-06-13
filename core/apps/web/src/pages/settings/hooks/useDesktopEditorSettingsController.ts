import { useEffect, useRef, useState, type Dispatch, type SetStateAction } from "react";
import {
  desktopGetEditorSettings,
  desktopUpdateEditorSettings,
  type DesktopEditorSettings,
} from "../../../utils/desktop";
import { errorMessage } from "../../../utils/errorMessage";
import {
  desktopEditorSettingsEqual,
  normalizeDesktopEditorSettings,
} from "../SettingsPage.utils";

type DesktopEditorSettingsController = {
  editorSettings: DesktopEditorSettings;
  setEditorSettings: Dispatch<SetStateAction<DesktopEditorSettings>>;
  editorLoaded: boolean;
  editorSaving: boolean;
  editorError: string | null;
};

const DEFAULT_EDITOR_SETTINGS: DesktopEditorSettings = {
  target: "system",
  custom_command: "",
  remote_authority: "",
};

export function useDesktopEditorSettingsController(enabled: boolean): DesktopEditorSettingsController {
  const [editorSettings, setEditorSettings] = useState<DesktopEditorSettings>(DEFAULT_EDITOR_SETTINGS);
  const [editorLoaded, setEditorLoaded] = useState(false);
  const [editorSaving, setEditorSaving] = useState(false);
  const [editorError, setEditorError] = useState<string | null>(null);

  const editorHydrated = useRef(false);
  const lastSavedEditorSettingsRef = useRef<DesktopEditorSettings | null>(null);

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    desktopGetEditorSettings()
      .then((settings) => {
        if (cancelled) return;
        const normalized = normalizeDesktopEditorSettings(settings);
        setEditorSettings(normalized);
        lastSavedEditorSettingsRef.current = normalized;
        setEditorLoaded(true);
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        setEditorError(errorMessage(error));
        setEditorLoaded(true);
      });
    return () => {
      cancelled = true;
    };
  }, [enabled]);

  useEffect(() => {
    if (!enabled || !editorLoaded) return;
    if (!editorHydrated.current) {
      editorHydrated.current = true;
      if (!lastSavedEditorSettingsRef.current) {
        lastSavedEditorSettingsRef.current = normalizeDesktopEditorSettings(editorSettings);
      }
      return;
    }
    if (desktopEditorSettingsEqual(editorSettings, lastSavedEditorSettingsRef.current)) return;

    const pendingSettings = normalizeDesktopEditorSettings(editorSettings);
    const timeout = window.setTimeout(() => {
      setEditorSaving(true);
      setEditorError(null);
      desktopUpdateEditorSettings(pendingSettings)
        .then((next) => {
          lastSavedEditorSettingsRef.current = normalizeDesktopEditorSettings(next);
          setEditorSettings((current) => (
            desktopEditorSettingsEqual(current, pendingSettings) ? next : current
          ));
        })
        .catch((error: unknown) => {
          setEditorError(errorMessage(error));
        })
        .finally(() => {
          setEditorSaving(false);
        });
    }, 350);

    return () => {
      window.clearTimeout(timeout);
    };
  }, [
    editorLoaded,
    editorSettings.custom_command,
    editorSettings.remote_authority,
    editorSettings.target,
    enabled,
  ]);

  return {
    editorSettings,
    setEditorSettings,
    editorLoaded,
    editorSaving,
    editorError,
  };
}
