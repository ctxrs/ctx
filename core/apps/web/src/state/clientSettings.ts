import { uiStateDelete, uiStateGet, uiStateSet } from "./uiStateStore";

export type ClientSettingsV1 = {
  v: 1;
  desktopNotifications: {
    turnCompleted: boolean;
  };
};

export type ClientSettingsV2 = {
  v: 2;
  desktopNotifications: {
    turnCompleted: boolean;
    turnFailed: boolean;
    badgeUnreadCount: boolean;
  };
};

export type ClientSettings = {
  v: 3;
  desktopNotifications: {
    turnCompleted: boolean;
    turnFailed: boolean;
    badgeUnreadCount: boolean;
  };
  telemetry: {
    clientEnabled: boolean;
  };
};

export type ClientSettingsState = {
  loaded: boolean;
  settings: ClientSettings;
};

const CLIENT_SETTINGS_KEY_V1 = "client.settings.v1";
const CLIENT_SETTINGS_KEY_V2 = "client.settings.v2";
const CLIENT_SETTINGS_KEY = "client.settings.v3";

const DEFAULT_SETTINGS: ClientSettings = {
  v: 3,
  desktopNotifications: {
    turnCompleted: true,
    turnFailed: true,
    badgeUnreadCount: true,
  },
  telemetry: {
    clientEnabled: true,
  },
};

let state: ClientSettingsState = {
  loaded: false,
  settings: DEFAULT_SETTINGS,
};

let loadPromise: Promise<ClientSettingsState> | null = null;
const listeners = new Set<() => void>();

const emit = () => {
  for (const listener of listeners) {
    listener();
  }
};

const normalizeV1 = (raw: unknown): ClientSettingsV1 | null => {
  if (!raw || typeof raw !== "object") return null;
  const rec = raw as Partial<ClientSettingsV1>;
  if (rec.v !== 1) return null;
  const desktopNotifications = (rec.desktopNotifications ?? {}) as Partial<ClientSettingsV1["desktopNotifications"]>;
  const turnCompleted =
    typeof desktopNotifications.turnCompleted === "boolean"
      ? desktopNotifications.turnCompleted
      : false;
  return {
    v: 1,
    desktopNotifications: {
      turnCompleted,
    },
  };
};

const normalizeV2 = (raw: unknown): ClientSettingsV2 | null => {
  if (!raw || typeof raw !== "object") return null;
  const rec = raw as Partial<ClientSettingsV2>;
  if (rec.v !== 2) return null;
  const desktopNotifications = (rec.desktopNotifications ?? {}) as Partial<ClientSettingsV2["desktopNotifications"]>;
  return {
    v: 2,
    desktopNotifications: {
      turnCompleted:
        typeof desktopNotifications.turnCompleted === "boolean"
          ? desktopNotifications.turnCompleted
          : DEFAULT_SETTINGS.desktopNotifications.turnCompleted,
      turnFailed:
        typeof desktopNotifications.turnFailed === "boolean"
          ? desktopNotifications.turnFailed
          : DEFAULT_SETTINGS.desktopNotifications.turnFailed,
      badgeUnreadCount:
        typeof desktopNotifications.badgeUnreadCount === "boolean"
          ? desktopNotifications.badgeUnreadCount
          : DEFAULT_SETTINGS.desktopNotifications.badgeUnreadCount,
    },
  };
};

const normalizeV3 = (raw: unknown): ClientSettings | null => {
  if (!raw || typeof raw !== "object") return null;
  const rec = raw as Partial<ClientSettings>;
  if (rec.v !== 3) return null;
  const desktopNotifications = (rec.desktopNotifications ?? {}) as Partial<ClientSettings["desktopNotifications"]>;
  const telemetry = (rec.telemetry ?? {}) as Partial<ClientSettings["telemetry"]>;
  return {
    v: 3,
    desktopNotifications: {
      turnCompleted:
        typeof desktopNotifications.turnCompleted === "boolean"
          ? desktopNotifications.turnCompleted
          : DEFAULT_SETTINGS.desktopNotifications.turnCompleted,
      turnFailed:
        typeof desktopNotifications.turnFailed === "boolean"
          ? desktopNotifications.turnFailed
          : DEFAULT_SETTINGS.desktopNotifications.turnFailed,
      badgeUnreadCount:
        typeof desktopNotifications.badgeUnreadCount === "boolean"
          ? desktopNotifications.badgeUnreadCount
          : DEFAULT_SETTINGS.desktopNotifications.badgeUnreadCount,
    },
    telemetry: {
      clientEnabled:
        typeof telemetry.clientEnabled === "boolean"
          ? telemetry.clientEnabled
          : DEFAULT_SETTINGS.telemetry.clientEnabled,
    },
  };
};

const migrateV1ToV2 = (legacy: ClientSettingsV1): ClientSettingsV2 => {
  const turnCompleted = legacy.desktopNotifications.turnCompleted;
  return {
    v: 2,
    desktopNotifications: {
      turnCompleted,
      turnFailed: turnCompleted,
      badgeUnreadCount: turnCompleted,
    },
  };
};

const migrateV2ToV3 = (legacy: ClientSettingsV2): ClientSettings => ({
  v: 3,
  desktopNotifications: legacy.desktopNotifications,
  telemetry: {
    clientEnabled: DEFAULT_SETTINGS.telemetry.clientEnabled,
  },
});

export function getClientSettingsState(): ClientSettingsState {
  return state;
}

export function getClientSettings(): ClientSettings {
  return state.settings;
}

export function subscribeClientSettings(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export async function loadClientSettings(): Promise<ClientSettingsState> {
  if (state.loaded) return state;
  if (loadPromise) return loadPromise;
  loadPromise = (async () => {
    let next = DEFAULT_SETTINGS;
    try {
      const rawV3 = await uiStateGet(CLIENT_SETTINGS_KEY);
      const normalizedV3 = normalizeV3(rawV3);
      if (normalizedV3) {
        next = normalizedV3;
      } else {
        const rawV2 = await uiStateGet(CLIENT_SETTINGS_KEY_V2);
        const normalizedV2 = normalizeV2(rawV2);
        if (normalizedV2) {
          next = migrateV2ToV3(normalizedV2);
          await uiStateSet(CLIENT_SETTINGS_KEY, next);
          await uiStateDelete(CLIENT_SETTINGS_KEY_V2);
        } else {
          const rawV1 = await uiStateGet(CLIENT_SETTINGS_KEY_V1);
          const legacy = normalizeV1(rawV1);
          if (legacy) {
            next = migrateV2ToV3(migrateV1ToV2(legacy));
            await uiStateSet(CLIENT_SETTINGS_KEY, next);
            await uiStateDelete(CLIENT_SETTINGS_KEY_V1);
          }
        }
      }
    } catch (err) {
      console.warn("client settings load failed, using defaults", err);
    }
    state = { loaded: true, settings: next };
    emit();
    return state;
  })();
  return loadPromise;
}

export async function updateClientSettings(
  patch: Omit<Partial<ClientSettings>, "desktopNotifications" | "telemetry"> & {
    desktopNotifications?: Partial<ClientSettings["desktopNotifications"]>;
    telemetry?: Partial<ClientSettings["telemetry"]>;
  },
): Promise<ClientSettingsState> {
  const next: ClientSettings = {
    ...state.settings,
    ...patch,
    v: 3,
    desktopNotifications: {
      ...state.settings.desktopNotifications,
      ...patch.desktopNotifications,
    },
    telemetry: {
      ...state.settings.telemetry,
      ...patch.telemetry,
    },
  };
  state = { loaded: true, settings: next };
  emit();
  try {
    await uiStateSet(CLIENT_SETTINGS_KEY, next);
  } catch (err) {
    console.warn("client settings save failed", err);
  }
  return state;
}
