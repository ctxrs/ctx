import type { Dispatch, SetStateAction } from "react";
import { useCallback, useEffect, useState, useSyncExternalStore } from "react";
import type { ThemeMode } from "../../../utils/theme";
import {
  applyTheme,
  resolveThemeMode,
  setStoredTheme,
  useThemeVariant,
} from "../../../utils/theme";
import { errorMessage } from "../../../utils/errorMessage";
import { isDesktopApp, type DesktopEditorSettings, type DesktopUpdateChannelSettings } from "../../../utils/desktop";
import {
  getDesktopNotificationPermission,
  requestDesktopNotificationPermission,
  type DesktopNotificationPermission,
} from "../../../utils/desktopNotifications";
import {
  getClientSettingsState,
  loadClientSettings,
  subscribeClientSettings,
  updateClientSettings,
  type ClientSettingsState,
} from "../../../state/clientSettings";
import { useDesktopEditorSettingsController } from "./useDesktopEditorSettingsController";
import { useDesktopUpdateChannelController } from "./useDesktopUpdateChannelController";

type SettingsGeneralPreferencesController = {
  theme: ThemeMode;
  onThemeChange: (next: ThemeMode) => void;
  editorSettings: DesktopEditorSettings;
  setEditorSettings: Dispatch<SetStateAction<DesktopEditorSettings>>;
  editorLoaded: boolean;
  editorError: string | null;
  updateChannel: DesktopUpdateChannelSettings["channel"];
  setUpdateChannel: (channel: DesktopUpdateChannelSettings["channel"]) => void;
  updateChannelLoaded: boolean;
  updateChannelError: string | null;
  showRemoteAuthority: boolean;
  isDesktopApp: () => boolean;
};

type SettingsNotificationPreferencesController = {
  isDesktopApp: () => boolean;
  clientSettingsState: ClientSettingsState;
  clientSettingsSaving: boolean;
  clientSettingsError: string | null;
  completedNotifications: boolean;
  failedNotifications: boolean;
  badgeUnreadCount: boolean;
  desktopNotificationPermission: DesktopNotificationPermission;
  desktopNotificationPermissionBusy: boolean;
  onToggleCompletedNotifications: (next: boolean) => Promise<void>;
  onToggleFailedNotifications: (next: boolean) => Promise<void>;
  onToggleBadgeUnreadCount: (next: boolean) => Promise<void>;
  onRequestDesktopNotificationPermission: () => Promise<void>;
};

type SettingsClientTelemetryController = {
  loaded: boolean;
  saving: boolean;
  error: string | null;
  enabled: boolean;
  setEnabled: (next: boolean) => Promise<void>;
};

type SettingsLocalPreferencesController = {
  themeVariant: "light" | "dark";
  general: SettingsGeneralPreferencesController;
  notifications: SettingsNotificationPreferencesController;
  telemetry: SettingsClientTelemetryController;
};

const vscodeRemoteTargets: DesktopEditorSettings["target"][] = [
  "vscode",
  "vscode_insiders",
  "cursor",
  "windsurf",
  "antigravity",
];

export function useSettingsLocalPreferencesController(): SettingsLocalPreferencesController {
  const [theme, setTheme] = useState<ThemeMode>(() => resolveThemeMode());
  const themeVariant = useThemeVariant();

  const {
    editorSettings,
    setEditorSettings,
    editorLoaded,
    editorError,
  } = useDesktopEditorSettingsController(isDesktopApp());
  const {
    updateChannel,
    setUpdateChannel,
    updateChannelLoaded,
    updateChannelError,
  } = useDesktopUpdateChannelController(isDesktopApp());

  const clientSettingsState = useSyncExternalStore(
    subscribeClientSettings,
    getClientSettingsState,
    getClientSettingsState,
  );
  const [clientSettingsSaving, setClientSettingsSaving] = useState(false);
  const [clientSettingsError, setClientSettingsError] = useState<string | null>(null);
  const [desktopNotificationPermission, setDesktopNotificationPermission] =
    useState<DesktopNotificationPermission>("unsupported");
  const [desktopNotificationPermissionBusy, setDesktopNotificationPermissionBusy] = useState(false);

  useEffect(() => {
    if (clientSettingsState.loaded) return;
    loadClientSettings().catch((error) => {
      setClientSettingsError(error?.message ?? String(error));
    });
  }, [clientSettingsState.loaded]);

  const refreshDesktopNotificationPermission = useCallback(async () => {
    if (!isDesktopApp()) {
      setDesktopNotificationPermission("unsupported");
      return;
    }

    setDesktopNotificationPermissionBusy(true);
    try {
      setDesktopNotificationPermission(await getDesktopNotificationPermission());
    } catch (error: unknown) {
      setClientSettingsError(errorMessage(error));
    } finally {
      setDesktopNotificationPermissionBusy(false);
    }
  }, []);

  useEffect(() => {
    void refreshDesktopNotificationPermission();
  }, [refreshDesktopNotificationPermission]);

  const handleUpdateClientSettings = useCallback(
    async (
      patch: Parameters<typeof updateClientSettings>[0],
    ) => {
      if (clientSettingsSaving) return;
      setClientSettingsSaving(true);
      setClientSettingsError(null);
      try {
        await updateClientSettings(patch);
      } catch (error: unknown) {
        setClientSettingsError(errorMessage(error));
      } finally {
        setClientSettingsSaving(false);
      }
    },
    [clientSettingsSaving],
  );

  const handleRequestDesktopNotificationPermission = useCallback(async () => {
    if (!isDesktopApp()) return;
    setClientSettingsError(null);
    setDesktopNotificationPermissionBusy(true);
    try {
      const permission = await requestDesktopNotificationPermission();
      setDesktopNotificationPermission(permission);
      if (permission === "denied") {
        setClientSettingsError(
          "Notification permission denied. Re-enable it in System Settings if the OS does not re-prompt.",
        );
      }
    } catch (error: unknown) {
      setClientSettingsError(errorMessage(error));
    } finally {
      setDesktopNotificationPermissionBusy(false);
    }
  }, []);

  const onThemeChange = useCallback((next: ThemeMode) => {
    setTheme(next);
    applyTheme(next);
    setStoredTheme(next);
  }, []);

  return {
    themeVariant,
    general: {
      theme,
      onThemeChange,
      editorSettings,
      setEditorSettings,
      editorLoaded,
      editorError,
      updateChannel,
      setUpdateChannel,
      updateChannelLoaded,
      updateChannelError,
      showRemoteAuthority: vscodeRemoteTargets.includes(editorSettings.target),
      isDesktopApp,
    },
    notifications: {
      isDesktopApp,
      clientSettingsState,
      clientSettingsSaving,
      clientSettingsError,
      completedNotifications: clientSettingsState.settings.desktopNotifications.turnCompleted,
      failedNotifications: clientSettingsState.settings.desktopNotifications.turnFailed,
      badgeUnreadCount: clientSettingsState.settings.desktopNotifications.badgeUnreadCount,
      desktopNotificationPermission,
      desktopNotificationPermissionBusy,
      onToggleCompletedNotifications: async (next) => {
        await handleUpdateClientSettings({ desktopNotifications: { turnCompleted: next } });
      },
      onToggleFailedNotifications: async (next) => {
        await handleUpdateClientSettings({ desktopNotifications: { turnFailed: next } });
      },
      onToggleBadgeUnreadCount: async (next) => {
        await handleUpdateClientSettings({ desktopNotifications: { badgeUnreadCount: next } });
      },
      onRequestDesktopNotificationPermission: handleRequestDesktopNotificationPermission,
    },
    telemetry: {
      loaded: clientSettingsState.loaded,
      saving: clientSettingsSaving,
      error: clientSettingsError,
      enabled: clientSettingsState.settings.telemetry.clientEnabled,
      setEnabled: async (next) => {
        await handleUpdateClientSettings({
          telemetry: {
            clientEnabled: next,
          },
        });
      },
    },
  };
}

export type {
  SettingsClientTelemetryController,
  SettingsGeneralPreferencesController,
  SettingsLocalPreferencesController,
  SettingsNotificationPreferencesController,
};
