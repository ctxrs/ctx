import { act, render, waitFor } from "@testing-library/react";
import { Fragment, createElement, useEffect } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  AmpAccountsResponse,
  HarnessProviderSourceConfig,
  ProvidersBootstrapResponse,
  ProviderAuthCheck,
  ProviderStatus,
} from "../../../api/client";
import {
  deleteAmpAccount,
  getAmpLogin,
  getClaudeLogin,
  getCodexLogin,
  getCursorLogin,
  getGeminiLogin,
  getKimiLogin,
  installAllProviders,
  listAmpAccounts,
  listClaudeAccounts,
  listCodexAccounts,
  listCopilotAccounts,
  listCursorAccounts,
  listGeminiAccounts,
  listKimiAccounts,
  listMistralAccounts,
  listQwenAccounts,
  getProviderOptions,
  selectProviderHarnessSource,
  setAmpActiveAccount,
  setCodexActiveAccount,
  startAmpLogin,
  startClaudeLogin,
  startCodexLogin,
  startCursorLogin,
  startGeminiLogin,
  startKimiLogin,
  upsertProviderHarnessEndpoint,
  verifyProviderForWorkspace,
} from "../../../api/client";
import {
  invalidateHostProvidersBootstrap,
  invalidateProvidersBootstrap,
  loadHostProvidersBootstrap,
  loadProvidersBootstrap,
  refreshHostProvidersBootstrap,
  refreshProvidersBootstrap,
  refreshProvidersBootstrapForScope,
} from "../../../state/providersBootstrapStore";
import { setDaemonConnection } from "../../../api/daemonConnection";
import {
  extractGithubDeviceCodeFromAuthUrl,
  resolveUpsertedEndpoint,
  resolveHarnessAuthModalInitialStage,
  shouldSkipDuplicateAmpLoginStart,
  shouldAutoOpenKimiAuthUrl,
  shouldOpenPolledAuthUrlForStatus,
  shouldAutoOpenCopilotAuthUrl,
  supportsHarnessSubscriptionAuth,
  toErrorObject,
  useHarnessAuthenticationController,
} from "./useHarnessAuthenticationController";
import { supportsHarnessEndpointConfigStatic, takeNextAuthUrlToOpen } from "./harnessAuth/capabilities";
import { resetProviderOnboardingCoordinatorForTests } from "../../../state/providerOnboardingCoordinator";
import { createDesktopLocalDaemonTargetScope } from "../../../state/scopeIdentity";
import { getProviderOwnerScopeKeyOrNull } from "../../../state/providerScopeAdapters";
import type { HarnessAuthRow } from "../harnessAuthRows";
import {
  desktopGetConnection,
  desktopStartCodexLoginRelay,
  isDesktopApp,
  openExternalLink,
} from "../../../utils/desktop";
import { readAcknowledgedProviderRuntimeWarningIds } from "../../../utils/providerRuntimeWarnings";

const analyticsMocks = vi.hoisted(() => ({
  trackFeatureUsed: vi.fn(),
  trackProviderAuthCompleted: vi.fn(),
  trackProviderAuthFailed: vi.fn(),
  trackProviderAuthStarted: vi.fn(),
}));

vi.mock("../../../utils/analytics", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../utils/analytics")>();
  return {
    ...actual,
    ...analyticsMocks,
  };
});

const bootstrapMockState = vi.hoisted(() => ({
  bootstrapStateByWorkspace: new Map<string, ProvidersBootstrapResponse>(),
  bootstrapLoadQueueByWorkspace: new Map<string, Array<ProvidersBootstrapResponse | Error>>(),
  bootstrapRefreshQueueByWorkspace: new Map<string, Array<ProvidersBootstrapResponse | Error>>(),
  bootstrapListenersByWorkspace: new Map<string, Set<() => void>>(),
  hostBootstrapState: null as ProvidersBootstrapResponse | null,
  hostBootstrapLoadQueue: [] as Array<ProvidersBootstrapResponse | Error>,
  hostBootstrapRefreshQueue: [] as Array<ProvidersBootstrapResponse | Error>,
  hostBootstrapListeners: new Set<() => void>(),
}));

const getBootstrapListeners = (workspaceId: string): Set<() => void> => {
  let listeners = bootstrapMockState.bootstrapListenersByWorkspace.get(workspaceId);
  if (!listeners) {
    listeners = new Set();
    bootstrapMockState.bootstrapListenersByWorkspace.set(workspaceId, listeners);
  }
  return listeners;
};

const setBootstrapSnapshot = (workspaceId: string, next: ProvidersBootstrapResponse): ProvidersBootstrapResponse => {
  bootstrapMockState.bootstrapStateByWorkspace.set(workspaceId, next);
  for (const listener of getBootstrapListeners(workspaceId)) {
    listener();
  }
  return next;
};

const queueBootstrapLoad = (workspaceId: string, ...entries: Array<ProvidersBootstrapResponse | Error>): void => {
  bootstrapMockState.bootstrapLoadQueueByWorkspace.set(workspaceId, entries);
};

const queueBootstrapRefresh = (workspaceId: string, ...entries: Array<ProvidersBootstrapResponse | Error>): void => {
  bootstrapMockState.bootstrapRefreshQueueByWorkspace.set(workspaceId, entries);
};

const consumeBootstrapQueue = (
  workspaceId: string,
  queueByWorkspace: Map<string, Array<ProvidersBootstrapResponse | Error>>,
  empty: ProvidersBootstrapResponse,
): ProvidersBootstrapResponse => {
  const queue = queueByWorkspace.get(workspaceId);
  if (queue && queue.length > 0) {
    const next = queue.shift()!;
    if (queue.length === 0) {
      queueByWorkspace.delete(workspaceId);
    }
    if (next instanceof Error) {
      throw next;
    }
    return setBootstrapSnapshot(workspaceId, next);
  }
  return bootstrapMockState.bootstrapStateByWorkspace.get(workspaceId) ?? empty;
};

const setHostBootstrapSnapshot = (next: ProvidersBootstrapResponse): ProvidersBootstrapResponse => {
  bootstrapMockState.hostBootstrapState = next;
  for (const listener of bootstrapMockState.hostBootstrapListeners) {
    listener();
  }
  return next;
};

const queueHostBootstrapLoad = (...entries: Array<ProvidersBootstrapResponse | Error>): void => {
  bootstrapMockState.hostBootstrapLoadQueue = [...entries];
};

const queueHostBootstrapRefresh = (...entries: Array<ProvidersBootstrapResponse | Error>): void => {
  bootstrapMockState.hostBootstrapRefreshQueue = [...entries];
};

const consumeHostBootstrapQueue = (
  queueKey: "hostBootstrapLoadQueue" | "hostBootstrapRefreshQueue",
  empty: ProvidersBootstrapResponse,
): ProvidersBootstrapResponse => {
  const queue = bootstrapMockState[queueKey];
  if (queue.length > 0) {
    const next = queue.shift()!;
    if (next instanceof Error) {
      throw next;
    }
    return setHostBootstrapSnapshot(next);
  }
  return bootstrapMockState.hostBootstrapState ?? empty;
};

vi.mock("../../../api/client", async (importOriginal) => {
  const original = await importOriginal<typeof import("../../../api/client")>();
  return {
    ...original,
    deleteAmpAccount: vi.fn(),
    getAmpLogin: vi.fn(),
    getClaudeLogin: vi.fn(),
    getCodexLogin: vi.fn(),
    getCursorLogin: vi.fn(),
    getGeminiLogin: vi.fn(),
    getKimiLogin: vi.fn(),
    installAllProviders: vi.fn(),
    listAmpAccounts: vi.fn(),
    listClaudeAccounts: vi.fn(),
    listCodexAccounts: vi.fn(),
    listCopilotAccounts: vi.fn(),
    listCursorAccounts: vi.fn(),
    listGeminiAccounts: vi.fn(),
    listKimiAccounts: vi.fn(),
    listMistralAccounts: vi.fn(),
    listQwenAccounts: vi.fn(),
    getProviderOptions: vi.fn(),
    selectProviderHarnessSource: vi.fn(),
    setAmpActiveAccount: vi.fn(),
    setCodexActiveAccount: vi.fn(),
    startAmpLogin: vi.fn(),
    startClaudeLogin: vi.fn(),
    startCodexLogin: vi.fn(),
    startCursorLogin: vi.fn(),
    startGeminiLogin: vi.fn(),
    startKimiLogin: vi.fn(),
    upsertProviderHarnessEndpoint: vi.fn(),
    verifyProviderForWorkspace: vi.fn(),
  };
});

vi.mock("../../../state/providersBootstrapStore", async (importOriginal) => {
  const original = await importOriginal<typeof import("../../../state/providersBootstrapStore")>();
  return {
    ...original,
    getHostProvidersBootstrapSnapshot: vi.fn(() =>
      bootstrapMockState.hostBootstrapState ?? original.EMPTY_PROVIDERS_BOOTSTRAP),
    getProvidersBootstrapSnapshot: vi.fn((workspaceId: string) =>
      bootstrapMockState.bootstrapStateByWorkspace.get(workspaceId) ?? original.EMPTY_PROVIDERS_BOOTSTRAP),
    getProvidersBootstrapSnapshotForScope: vi.fn((ownerScope: { kind: "host" | "workspace"; workspaceId?: string }) =>
      ownerScope.kind === "workspace"
        ? bootstrapMockState.bootstrapStateByWorkspace.get(ownerScope.workspaceId ?? "") ?? original.EMPTY_PROVIDERS_BOOTSTRAP
        : bootstrapMockState.hostBootstrapState ?? original.EMPTY_PROVIDERS_BOOTSTRAP),
    hasCachedHostProvidersBootstrap: vi.fn(() => bootstrapMockState.hostBootstrapState !== null),
    invalidateHostProvidersBootstrap: vi.fn(),
    invalidateProvidersBootstrap: vi.fn(),
    loadHostProvidersBootstrap: vi.fn(async () =>
      consumeHostBootstrapQueue("hostBootstrapLoadQueue", original.EMPTY_PROVIDERS_BOOTSTRAP)),
    loadProvidersBootstrap: vi.fn(async (workspaceId: string) =>
      consumeBootstrapQueue(workspaceId, bootstrapMockState.bootstrapLoadQueueByWorkspace, original.EMPTY_PROVIDERS_BOOTSTRAP)),
    loadProvidersBootstrapForScope: vi.fn(async (ownerScope: { kind: "host" | "workspace"; workspaceId?: string }) =>
      ownerScope.kind === "workspace"
        ? consumeBootstrapQueue(
          ownerScope.workspaceId ?? "",
          bootstrapMockState.bootstrapLoadQueueByWorkspace,
          original.EMPTY_PROVIDERS_BOOTSTRAP,
        )
        : consumeHostBootstrapQueue("hostBootstrapLoadQueue", original.EMPTY_PROVIDERS_BOOTSTRAP)),
    refreshHostProvidersBootstrap: vi.fn(async () =>
      consumeHostBootstrapQueue("hostBootstrapRefreshQueue", original.EMPTY_PROVIDERS_BOOTSTRAP)),
    refreshProvidersBootstrap: vi.fn(async (workspaceId: string) =>
      consumeBootstrapQueue(workspaceId, bootstrapMockState.bootstrapRefreshQueueByWorkspace, original.EMPTY_PROVIDERS_BOOTSTRAP)),
    refreshProvidersBootstrapForScope: vi.fn(async (ownerScope: { kind: "host" | "workspace"; workspaceId?: string }) =>
      ownerScope.kind === "workspace"
        ? consumeBootstrapQueue(
          ownerScope.workspaceId ?? "",
          bootstrapMockState.bootstrapRefreshQueueByWorkspace,
          original.EMPTY_PROVIDERS_BOOTSTRAP,
        )
        : consumeHostBootstrapQueue("hostBootstrapRefreshQueue", original.EMPTY_PROVIDERS_BOOTSTRAP)),
    subscribeHostProvidersBootstrap: vi.fn((listener: () => void) => {
      bootstrapMockState.hostBootstrapListeners.add(listener);
      return () => {
        bootstrapMockState.hostBootstrapListeners.delete(listener);
      };
    }),
    subscribeProvidersBootstrap: vi.fn((workspaceId: string, listener: () => void) => {
      const listeners = getBootstrapListeners(workspaceId);
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    }),
    subscribeProvidersBootstrapForScope: vi.fn((ownerScope: { kind: "host" | "workspace"; workspaceId?: string }, listener: () => void) => {
      if (ownerScope.kind === "workspace") {
        const listeners = getBootstrapListeners(ownerScope.workspaceId ?? "");
        listeners.add(listener);
        return () => {
          listeners.delete(listener);
        };
      }
      bootstrapMockState.hostBootstrapListeners.add(listener);
      return () => {
        bootstrapMockState.hostBootstrapListeners.delete(listener);
      };
    }),
    updateHostProvidersBootstrap: vi.fn((updater: (current: ProvidersBootstrapResponse) => ProvidersBootstrapResponse) =>
      setHostBootstrapSnapshot(
        updater(bootstrapMockState.hostBootstrapState ?? original.EMPTY_PROVIDERS_BOOTSTRAP),
      )),
    updateProvidersBootstrap: vi.fn((workspaceId: string, updater: (current: ProvidersBootstrapResponse) => ProvidersBootstrapResponse) =>
      setBootstrapSnapshot(
        workspaceId,
        updater(bootstrapMockState.bootstrapStateByWorkspace.get(workspaceId) ?? original.EMPTY_PROVIDERS_BOOTSTRAP),
      )),
    updateProvidersBootstrapForScope: vi.fn((
      ownerScope: { kind: "host" | "workspace"; workspaceId?: string },
      updater: (current: ProvidersBootstrapResponse) => ProvidersBootstrapResponse,
    ) => ownerScope.kind === "workspace"
      ? setBootstrapSnapshot(
        ownerScope.workspaceId ?? "",
        updater(bootstrapMockState.bootstrapStateByWorkspace.get(ownerScope.workspaceId ?? "") ?? original.EMPTY_PROVIDERS_BOOTSTRAP),
      )
      : setHostBootstrapSnapshot(
        updater(bootstrapMockState.hostBootstrapState ?? original.EMPTY_PROVIDERS_BOOTSTRAP),
      )),
  };
});

vi.mock("../../../state/providerInstallProgressStore", async (importOriginal) => {
  const original = await importOriginal<typeof import("../../../state/providerInstallProgressStore")>();
  return {
    ...original,
    getProviderInstallProgressSnapshot: vi.fn(() => ({})),
    getProviderInstallProgressSnapshotForScope: vi.fn(() => ({})),
    subscribeProviderInstallProgress: vi.fn(() => () => {}),
    subscribeProviderInstallProgressForScope: vi.fn(() => () => {}),
    upsertProviderInstallProgress: vi.fn(),
    upsertProviderInstallProgressForScope: vi.fn(),
  };
});

vi.mock("../../../utils/desktop", async (importOriginal) => {
  const original = await importOriginal<typeof import("../../../utils/desktop")>();
  return {
    ...original,
    desktopGetConnection: vi.fn(),
    desktopStartCodexLoginRelay: vi.fn(),
    isDesktopApp: vi.fn(() => false),
    openExternalLink: vi.fn(),
  };
});

type Controller = ReturnType<typeof useHarnessAuthenticationController>;

const requireController = (controller: Controller | null): Controller => {
  if (!controller) throw new Error("controller not ready");
  return controller;
};

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
};

const deferred = <T,>(): Deferred<T> => {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
};

const baseEndpoint = {
  id: "ep-old",
  provider_id: "codex",
  name: "Primary",
  base_url: "https://api.example.com/v1",
  api_shape: "openai_responses" as const,
  auth_type: "bearer",
  model_override: null,
  created_at: "2026-03-01T00:00:00Z",
  updated_at: "2026-03-01T00:00:00Z",
  last_verification_status: "unknown" as const,
  last_verification_at: null,
  last_error: null,
  has_api_key: true,
};

const baseCodexConfig: HarnessProviderSourceConfig = {
  provider_id: "codex",
  selected_source_kind: "subscription",
  selected_endpoint_id: null,
  endpoints: [baseEndpoint],
};

const baseAmpAccounts: AmpAccountsResponse = {
  active_account_id: "amp-1",
  accounts: [
    {
      id: "amp-1",
      label: "Amp One",
      created_at: "2026-03-01T00:00:00Z",
    },
    {
      id: "amp-2",
      label: "Amp Two",
      created_at: "2026-03-01T00:00:00Z",
    },
  ],
};

const makeBootstrap = (overrides?: Partial<ProvidersBootstrapResponse>): ProvidersBootstrapResponse => ({
  providers: [],
  provider_options: {},
  provider_harness_config: {
    codex: baseCodexConfig,
  },
  codex_accounts: {
    active_account_id: null,
    accounts: [],
    logins: [],
  },
  claude_accounts: {
    active_account_id: null,
    accounts: [],
  },
  gemini_accounts: {
    active_account_id: null,
    accounts: [],
  },
  qwen_accounts: {
    active_account_id: null,
    accounts: [],
  },
  kimi_accounts: {
    active_account_id: null,
    accounts: [],
  },
  mistral_accounts: {
    active_account_id: null,
    accounts: [],
  },
  copilot_accounts: {
    active_account_id: null,
    accounts: [],
  },
  cursor_accounts: {
    active_account_id: null,
    accounts: [],
  },
  amp_accounts: baseAmpAccounts,
  ...overrides,
});

function ControllerHarness({
  onChange,
  workspaceId = "ws-test",
}: {
  onChange: (controller: Controller) => void;
  workspaceId?: string | null;
}) {
  const controller = useHarnessAuthenticationController({
    workspaceId,
    enabled: true,
  });

  useEffect(() => {
    onChange(controller);
  }, [controller, onChange]);

  return null;
}

beforeEach(() => {
  resetProviderOnboardingCoordinatorForTests();
  bootstrapMockState.bootstrapStateByWorkspace.clear();
  bootstrapMockState.bootstrapLoadQueueByWorkspace.clear();
  bootstrapMockState.bootstrapRefreshQueueByWorkspace.clear();
  bootstrapMockState.bootstrapListenersByWorkspace.clear();
  bootstrapMockState.hostBootstrapState = null;
  bootstrapMockState.hostBootstrapLoadQueue = [];
  bootstrapMockState.hostBootstrapRefreshQueue = [];
  bootstrapMockState.hostBootstrapListeners.clear();
  vi.clearAllMocks();
  vi.mocked(deleteAmpAccount).mockReset();
  vi.mocked(getAmpLogin).mockReset();
  vi.mocked(getClaudeLogin).mockReset();
  vi.mocked(getCodexLogin).mockReset();
  vi.mocked(getCursorLogin).mockReset();
  vi.mocked(getGeminiLogin).mockReset();
  vi.mocked(getKimiLogin).mockReset();
  vi.mocked(installAllProviders).mockReset();
  vi.mocked(listAmpAccounts).mockReset();
  vi.mocked(listClaudeAccounts).mockReset();
  vi.mocked(listCodexAccounts).mockReset();
  vi.mocked(listCopilotAccounts).mockReset();
  vi.mocked(listCursorAccounts).mockReset();
  vi.mocked(listGeminiAccounts).mockReset();
  vi.mocked(listKimiAccounts).mockReset();
  vi.mocked(listMistralAccounts).mockReset();
  vi.mocked(listQwenAccounts).mockReset();
  vi.mocked(getProviderOptions).mockReset();
  vi.mocked(selectProviderHarnessSource).mockReset();
  vi.mocked(setAmpActiveAccount).mockReset();
  vi.mocked(setCodexActiveAccount).mockReset();
  vi.mocked(startAmpLogin).mockReset();
  vi.mocked(startClaudeLogin).mockReset();
  vi.mocked(startCodexLogin).mockReset();
  vi.mocked(startCursorLogin).mockReset();
  vi.mocked(startGeminiLogin).mockReset();
  vi.mocked(startKimiLogin).mockReset();
  vi.mocked(upsertProviderHarnessEndpoint).mockReset();
  vi.mocked(verifyProviderForWorkspace).mockReset();
  analyticsMocks.trackFeatureUsed.mockReset();
  analyticsMocks.trackProviderAuthCompleted.mockReset();
  analyticsMocks.trackProviderAuthFailed.mockReset();
  analyticsMocks.trackProviderAuthStarted.mockReset();
  vi.mocked(invalidateHostProvidersBootstrap).mockReset();
  vi.mocked(invalidateProvidersBootstrap).mockReset();
  vi.mocked(loadHostProvidersBootstrap).mockReset();
  vi.mocked(loadProvidersBootstrap).mockReset();
  vi.mocked(refreshHostProvidersBootstrap).mockReset();
  vi.mocked(refreshProvidersBootstrap).mockReset();
  vi.mocked(refreshProvidersBootstrapForScope).mockReset();
  vi.mocked(desktopGetConnection).mockReset();
  vi.mocked(desktopGetConnection).mockResolvedValue({
    kind: "local",
  } as Awaited<ReturnType<typeof desktopGetConnection>>);
  vi.mocked(desktopStartCodexLoginRelay).mockReset();
  vi.mocked(desktopStartCodexLoginRelay).mockResolvedValue(true);
  vi.mocked(isDesktopApp).mockReset();
  vi.mocked(isDesktopApp).mockReturnValue(false);
  vi.mocked(openExternalLink).mockReset();
  setBootstrapSnapshot("ws-test", makeBootstrap());
  setHostBootstrapSnapshot(makeBootstrap());
  vi.mocked(loadHostProvidersBootstrap).mockImplementation(async () =>
    consumeHostBootstrapQueue("hostBootstrapLoadQueue", makeBootstrap()));
  vi.mocked(loadProvidersBootstrap).mockImplementation(async (workspaceId: string) =>
    consumeBootstrapQueue(workspaceId, bootstrapMockState.bootstrapLoadQueueByWorkspace, makeBootstrap()));
  vi.mocked(selectProviderHarnessSource).mockResolvedValue(baseCodexConfig);
  vi.mocked(refreshHostProvidersBootstrap).mockImplementation(async () =>
    consumeHostBootstrapQueue("hostBootstrapRefreshQueue", makeBootstrap()));
  vi.mocked(refreshProvidersBootstrap).mockImplementation(async (workspaceId: string) =>
    consumeBootstrapQueue(workspaceId, bootstrapMockState.bootstrapRefreshQueueByWorkspace, makeBootstrap()));
  vi.mocked(refreshProvidersBootstrapForScope).mockImplementation(async (
    ownerScope: { kind: "host" | "workspace"; workspaceId?: string },
  ) => ownerScope.kind === "workspace"
    ? consumeBootstrapQueue(
      ownerScope.workspaceId ?? "",
      bootstrapMockState.bootstrapRefreshQueueByWorkspace,
      makeBootstrap(),
    )
    : consumeHostBootstrapQueue("hostBootstrapRefreshQueue", makeBootstrap()));
  vi.mocked(listAmpAccounts).mockResolvedValue(baseAmpAccounts);
  vi.mocked(listClaudeAccounts).mockResolvedValue({
    active_account_id: null,
    accounts: [],
  });
  vi.mocked(listCodexAccounts).mockResolvedValue({
    active_account_id: null,
    accounts: [],
    logins: [],
  });
  vi.mocked(listCopilotAccounts).mockResolvedValue({
    active_account_id: null,
    accounts: [],
  });
  vi.mocked(listCursorAccounts).mockResolvedValue({
    active_account_id: null,
    accounts: [],
  });
  vi.mocked(listGeminiAccounts).mockResolvedValue({
    active_account_id: null,
    accounts: [],
  });
  vi.mocked(listKimiAccounts).mockResolvedValue({
    active_account_id: null,
    accounts: [],
  });
  vi.mocked(listMistralAccounts).mockResolvedValue({
    active_account_id: null,
    accounts: [],
  });
  vi.mocked(listQwenAccounts).mockResolvedValue({
    active_account_id: null,
    accounts: [],
  });
  vi.mocked(getProviderOptions).mockImplementation(async (workspaceId: string, providerId: string) => ({
    provider_id: providerId,
    workspace_id: workspaceId,
    supports_load: false,
    auth_required: false,
    has_active_auth: true,
    auth_mode: "subscription",
    source: {
      provider_id: providerId,
      selected_source_kind: "subscription",
      selected_endpoint_id: null,
      endpoints: [],
    },
    probed_at: "2026-03-10T00:00:00.000Z",
  }));
  setDaemonConnection({
    baseUrl: "https://daemon-a.example",
    source: "test",
  });
  window.sessionStorage.clear();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("shouldSkipDuplicateAmpLoginStart", () => {
  it("skips duplicate Amp starts while one is in flight", () => {
    expect(shouldSkipDuplicateAmpLoginStart({
      providerId: "amp",
      ampLoginInFlight: true,
    })).toBe(true);
    expect(shouldSkipDuplicateAmpLoginStart({
      providerId: "amp",
      ampLoginInFlight: false,
    })).toBe(false);
    expect(shouldSkipDuplicateAmpLoginStart({
      providerId: "gemini",
      ampLoginInFlight: true,
    })).toBe(false);
  });
});

describe("shouldOpenPolledAuthUrlForStatus", () => {
  it("opens auth url only while status is pending", () => {
    expect(shouldOpenPolledAuthUrlForStatus("pending")).toBe(true);
    expect(shouldOpenPolledAuthUrlForStatus("success")).toBe(false);
    expect(shouldOpenPolledAuthUrlForStatus("failed")).toBe(false);
    expect(shouldOpenPolledAuthUrlForStatus("timeout")).toBe(false);
  });
});

describe("takeNextAuthUrlToOpen", () => {
  it("dedupes Claude auth urls by OAuth state instead of raw url", () => {
    const opened = new Set<string>();
    const first = "https://claude.ai/oauth/authorize?code=true&state=shared-state&redirect_uri=http%3A%2F%2Flocalhost%3A52731%2Fcallback";
    const second = "https://claude.ai/oauth/authorize?code=true&state=shared-state&redirect_uri=http%3A%2F%2Flocalhost%3A52732%2Fcallback";

    expect(takeNextAuthUrlToOpen(first, opened)).toBe(first);
    expect(takeNextAuthUrlToOpen(second, opened)).toBeNull();
  });

  it("still opens distinct Claude auth sessions", () => {
    const opened = new Set<string>();
    const first = "https://claude.ai/oauth/authorize?code=true&state=state-a&redirect_uri=http%3A%2F%2Flocalhost%3A52731%2Fcallback";
    const second = "https://claude.ai/oauth/authorize?code=true&state=state-b&redirect_uri=http%3A%2F%2Flocalhost%3A52732%2Fcallback";

    expect(takeNextAuthUrlToOpen(first, opened)).toBe(first);
    expect(takeNextAuthUrlToOpen(second, opened)).toBe(second);
  });
});

describe("shouldAutoOpenCopilotAuthUrl", () => {
  it("does not auto-open copilot auth urls from the web app", () => {
    expect(shouldAutoOpenCopilotAuthUrl("https://github.com/login/device")).toBe(false);
    expect(
      shouldAutoOpenCopilotAuthUrl("https://github.com/login/device?user_code=ABCD-1234"),
    ).toBe(false);
    expect(shouldAutoOpenCopilotAuthUrl("https://github.com/login/oauth/authorize?client_id=abc")).toBe(
      false,
    );
    expect(shouldAutoOpenCopilotAuthUrl("https://example.com/auth")).toBe(false);
  });
});

describe("extractGithubDeviceCodeFromAuthUrl", () => {
  it("extracts user_code from github device url", () => {
    expect(
      extractGithubDeviceCodeFromAuthUrl("https://github.com/login/device?user_code=ABCD-1234"),
    ).toBe("ABCD-1234");
  });

  it("returns null for non-device urls", () => {
    expect(extractGithubDeviceCodeFromAuthUrl("https://github.com/login/device")).toBeNull();
    expect(extractGithubDeviceCodeFromAuthUrl("https://example.com/login/device?user_code=ABCD-1234")).toBeNull();
    expect(extractGithubDeviceCodeFromAuthUrl("")).toBeNull();
  });
});

describe("resolveUpsertedEndpoint", () => {
  it("reuses the requested endpoint id during retry", () => {
    const endpoint = resolveUpsertedEndpoint({
      requestedEndpointId: "ep-2",
      previousEndpointIds: new Set(["ep-1", "ep-2"]),
      nextEndpoints: [
        {
          id: "ep-1",
          provider_id: "codex",
          name: "Primary",
          base_url: "https://api.example.com/v1",
          api_shape: "openai_responses",
          auth_type: "bearer",
          model_override: null,
          created_at: "2026-02-20T00:00:00Z",
          updated_at: "2026-02-20T00:00:00Z",
          last_verification_status: "unknown",
          last_verification_at: null,
          last_error: null,
          has_api_key: true,
        },
        {
          id: "ep-2",
          provider_id: "codex",
          name: "Primary",
          base_url: "https://api.example.com/v1",
          api_shape: "openai_responses",
          auth_type: "bearer",
          model_override: null,
          created_at: "2026-02-20T00:00:00Z",
          updated_at: "2026-02-20T00:00:00Z",
          last_verification_status: "unknown",
          last_verification_at: null,
          last_error: null,
          has_api_key: true,
        },
      ],
      name: "Primary",
      normalizedBase: "https://api.example.com/v1",
      geminiAuthType: null,
    });
    expect(endpoint?.id).toBe("ep-2");
  });

  it("selects the newly created endpoint when creating for the first time", () => {
    const endpoint = resolveUpsertedEndpoint({
      requestedEndpointId: null,
      previousEndpointIds: new Set(["ep-1"]),
      nextEndpoints: [
        {
          id: "ep-1",
          provider_id: "codex",
          name: "Existing",
          base_url: "https://api.example.com/v1",
          api_shape: "openai_responses",
          auth_type: "bearer",
          model_override: null,
          created_at: "2026-02-20T00:00:00Z",
          updated_at: "2026-02-20T00:00:00Z",
          last_verification_status: "unknown",
          last_verification_at: null,
          last_error: null,
          has_api_key: true,
        },
        {
          id: "ep-2",
          provider_id: "codex",
          name: "OpenRouter",
          base_url: "https://openrouter.ai/api/v1",
          api_shape: "openai_responses",
          auth_type: "bearer",
          model_override: null,
          created_at: "2026-02-20T00:00:00Z",
          updated_at: "2026-02-20T00:00:00Z",
          last_verification_status: "unknown",
          last_verification_at: null,
          last_error: null,
          has_api_key: true,
        },
      ],
      name: "OpenRouter",
      normalizedBase: "https://openrouter.ai/api/v1",
      geminiAuthType: null,
    });
    expect(endpoint?.id).toBe("ep-2");
  });
});

describe("shouldAutoOpenKimiAuthUrl", () => {
  it("opens Kimi browser auth automatically", () => {
    expect(shouldAutoOpenKimiAuthUrl()).toBe(true);
  });
});

describe("supportsHarnessSubscriptionAuth", () => {
  it("returns false for API-key-only providers", () => {
    expect(supportsHarnessSubscriptionAuth("opencode")).toBe(false);
    expect(supportsHarnessSubscriptionAuth("pi")).toBe(false);
    expect(supportsHarnessSubscriptionAuth("mistral")).toBe(false);
  });

  it("returns true for subscription-capable providers", () => {
    expect(supportsHarnessSubscriptionAuth("codex")).toBe(true);
    expect(supportsHarnessSubscriptionAuth("claude-crp")).toBe(true);
    expect(supportsHarnessSubscriptionAuth("gemini")).toBe(true);
  });

  it("returns true for cursor now that managed browser auth is restored", () => {
    expect(supportsHarnessSubscriptionAuth("cursor")).toBe(true);
  });
});

describe("supportsHarnessEndpointConfigStatic", () => {
  it("returns true for upstream ACP endpoint-capable harnesses", () => {
    expect(supportsHarnessEndpointConfigStatic("cline")).toBe(true);
    expect(supportsHarnessEndpointConfigStatic("goose")).toBe(true);
    expect(supportsHarnessEndpointConfigStatic("openhands")).toBe(true);
  });

  it("returns true for supported endpoint-capable harnesses", () => {
    expect(supportsHarnessEndpointConfigStatic("opencode")).toBe(true);
    expect(supportsHarnessEndpointConfigStatic("pi")).toBe(true);
  });
});

describe("resolveHarnessAuthModalInitialStage", () => {
  it("routes API-key-only providers directly to api_key", () => {
    expect(resolveHarnessAuthModalInitialStage("cline")).toBe("api_key");
    expect(resolveHarnessAuthModalInitialStage("goose")).toBe("api_key");
    expect(resolveHarnessAuthModalInitialStage("openhands")).toBe("api_key");
    expect(resolveHarnessAuthModalInitialStage("opencode")).toBe("api_key");
    expect(resolveHarnessAuthModalInitialStage("pi")).toBe("api_key");
  });

  it("routes subscription-only providers directly to subscription", () => {
    expect(resolveHarnessAuthModalInitialStage("amp")).toBe("subscription");
    expect(resolveHarnessAuthModalInitialStage("auggie")).toBe("subscription");
  });

  it("keeps choose stage for providers supporting both methods", () => {
    expect(resolveHarnessAuthModalInitialStage("codex")).toBe("choose");
    expect(resolveHarnessAuthModalInitialStage("claude-crp")).toBe("choose");
  });

  it("keeps cursor on choose stage when both subscription and API-key auth are available", () => {
    expect(resolveHarnessAuthModalInitialStage("cursor")).toBe("choose");
  });
});

describe("useHarnessAuthenticationController", () => {
  it("waits for a daemon target scope before loading workspace provider state", async () => {
    let controller: Controller | null = null;
    queueBootstrapLoad("ws-test", makeBootstrap({
      providers: [
        {
          provider_id: "codex",
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
        } satisfies ProviderStatus,
      ],
    }));
    setDaemonConnection({
      baseUrl: "https://desktop-daemon.example",
      source: "desktop",
      targetScope: null,
    });

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    expect(requireController(controller).providers).toEqual([]);
    expect(vi.mocked(loadProvidersBootstrap)).not.toHaveBeenCalled();

    act(() => {
      setDaemonConnection({
        baseUrl: "https://desktop-daemon.example",
        source: "desktop",
        targetScope: createDesktopLocalDaemonTargetScope(),
      });
    });

    await waitFor(() => {
      expect(vi.mocked(loadProvidersBootstrap)).toHaveBeenCalledWith("ws-test");
      expect(controller?.providers[0]?.provider_id).toBe("codex");
    });
  });

  it("acknowledges the current actionable warning set when installing all from settings", async () => {
    const workspaceId = "ws-install-all";
    let controller: Controller | null = null;
    const providers: ProviderStatus[] = [
      {
        provider_id: "codex",
        installed: true,
        health: "ok",
        diagnostics: [],
        details: {
          install_supported: "true",
          matrix_update_available: "true",
        },
        usability: {
          usable: true,
          status: "ready",
          blocking_provider_ids: [],
          recommended_action: "none",
        },
      },
      {
        provider_id: "gemini",
        installed: false,
        health: "missing",
        diagnostics: [],
        details: {
          install_supported: "true",
          matrix_update_available: "true",
        },
        usability: {
          usable: false,
          status: "blocked",
          reason: "runtime not installed",
          blocking_provider_ids: [],
          recommended_action: "install",
        },
      },
    ];
    const bootstrap = makeBootstrap({ providers });
    queueBootstrapLoad(workspaceId, bootstrap);
    setBootstrapSnapshot(workspaceId, bootstrap);
    vi.mocked(installAllProviders).mockResolvedValue([
      {
        provider_id: "codex",
        install_id: "install-codex",
        target: "host",
      },
      {
        provider_id: "gemini",
        install_id: "install-gemini",
        target: "host",
      },
    ]);
    setDaemonConnection({
      baseUrl: "https://desktop-daemon.example",
      source: "desktop",
      targetScope: createDesktopLocalDaemonTargetScope(),
    });

    render(createElement(ControllerHarness, {
      workspaceId,
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller?.providers).toHaveLength(2);
    });

    const ownerScopeKey = getProviderOwnerScopeKeyOrNull(workspaceId);
    expect(ownerScopeKey).not.toBeNull();

    await act(async () => {
      await controller?.onInstallAll();
    });

    expect(vi.mocked(installAllProviders)).toHaveBeenCalledWith("host");
    expect(readAcknowledgedProviderRuntimeWarningIds(ownerScopeKey)).toEqual(["codex"]);
  });

  it("submits Gemini Vertex service-account endpoint auth without a base URL", async () => {
    let controller: Controller | null = null;
    const vertexEndpoint = {
      ...baseEndpoint,
      id: "gemini-vertex-1",
      provider_id: "gemini",
      name: "Gemini Vertex",
      base_url: null,
      auth_type: "vertex_ai",
      model_override: null,
    };
    const selectedEndpointConfig: HarnessProviderSourceConfig = {
      provider_id: "gemini",
      selected_source_kind: "endpoint",
      selected_endpoint_id: vertexEndpoint.id,
      endpoints: [vertexEndpoint],
    };

    setBootstrapSnapshot("ws-test", makeBootstrap({
      provider_harness_config: {
        codex: baseCodexConfig,
        gemini: selectedEndpointConfig,
      },
    }));
    queueBootstrapRefresh("ws-test", makeBootstrap({
      provider_harness_config: {
        codex: baseCodexConfig,
        gemini: selectedEndpointConfig,
      },
    }));
    vi.mocked(upsertProviderHarnessEndpoint).mockResolvedValue(selectedEndpointConfig);
    vi.mocked(selectProviderHarnessSource).mockResolvedValue(selectedEndpointConfig);
    vi.mocked(verifyProviderForWorkspace).mockResolvedValue({
      provider_id: "gemini",
      workspace_id: "ws-test",
      status: "ok",
      message: undefined,
    });

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      controller?.openHarnessAuthModal("gemini");
    });
    await waitFor(() => {
      expect(controller?.harnessAuthModal?.provider_id).toBe("gemini");
      expect(controller?.harnessAuthModal?.base_url).toBe("");
    });

    await act(async () => {
      controller?.patchHarnessAuthModal({
        stage: "api_key",
        endpoint_provider_id: "google_vertex",
        gemini_endpoint_auth_type: "vertex_ai",
        service_account_json:
          '{"type":"service_account","project_id":"vertex-project","private_key_id":"key-id","private_key":"-----BEGIN PRIVATE KEY-----\\nabc\\n-----END PRIVATE KEY-----\\n","client_email":"ctx-vertex@test.iam.gserviceaccount.com","client_id":"1234567890"}',
        project_id: "vertex-project",
        location: "global",
        base_url: "not_used",
      });
    });
    await act(async () => {
      await controller?.submitHarnessApiKeyModal();
    });

    await waitFor(() => {
      expect(vi.mocked(upsertProviderHarnessEndpoint)).toHaveBeenCalledWith("gemini", expect.objectContaining({
        base_url: null,
        auth_type: "vertex_ai",
        service_account_json: expect.stringContaining('"type":"service_account"'),
        project_id: "vertex-project",
        location: "global",
      }));
    });
  });

  it("tracks provider source selection when choosing an existing endpoint row", async () => {
    let controller: Controller | null = null;
    const endpointRow: HarnessAuthRow = {
      key: "endpoint:ep-old",
      kind: "api_key",
      label: "Primary",
      active: false,
      selectable: true,
      endpoint_id: "ep-old",
    };
    const selectedEndpointConfig: HarnessProviderSourceConfig = {
      ...baseCodexConfig,
      selected_source_kind: "endpoint",
      selected_endpoint_id: "ep-old",
    };

    setBootstrapSnapshot("ws-test", makeBootstrap());
    vi.mocked(selectProviderHarnessSource).mockResolvedValue(selectedEndpointConfig);

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      await controller?.onSelectHarnessAuthRow("codex", endpointRow);
    });

    await waitFor(() => {
      expect(vi.mocked(selectProviderHarnessSource)).toHaveBeenCalledWith("codex", "endpoint", "ep-old");
      expect(analyticsMocks.trackFeatureUsed).toHaveBeenCalledWith("provider_source_selected", {
        provider_id: "codex",
        source_kind: "endpoint",
        scope_kind: "workspace",
      });
    });
  });

  it("restores the previous provider source when endpoint verification fails", async () => {
    let controller: Controller | null = null;
    const freshEndpoint = {
      ...baseEndpoint,
      id: "ep-new",
      name: "Secondary",
      updated_at: "2026-03-02T00:00:00Z",
    };
    const afterUpsert: HarnessProviderSourceConfig = {
      ...baseCodexConfig,
      endpoints: [...baseCodexConfig.endpoints, freshEndpoint],
    };
    const selectedEndpointConfig: HarnessProviderSourceConfig = {
      ...afterUpsert,
      selected_source_kind: "endpoint",
      selected_endpoint_id: freshEndpoint.id,
    };
    const verifyFailure: ProviderAuthCheck = {
      provider_id: "codex",
      workspace_id: "ws-test",
      status: "failed",
      message: "bad endpoint",
    };

    vi.mocked(upsertProviderHarnessEndpoint).mockResolvedValue(afterUpsert);
    vi.mocked(selectProviderHarnessSource)
      .mockResolvedValueOnce(selectedEndpointConfig)
      .mockResolvedValueOnce(baseCodexConfig);
    queueBootstrapRefresh(
      "ws-test",
      makeBootstrap({
        provider_harness_config: {
          codex: selectedEndpointConfig,
        },
      }),
      makeBootstrap(),
    );
    vi.mocked(verifyProviderForWorkspace).mockResolvedValue(verifyFailure);

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
      expect(vi.mocked(loadProvidersBootstrap)).toHaveBeenCalledWith("ws-test");
    });

    await act(async () => {
      controller?.openHarnessAuthModal("codex");
    });
    await act(async () => {
      controller?.patchHarnessAuthModal({
        stage: "api_key",
        api_key: "sk-test",
      });
    });
    await act(async () => {
      await controller?.submitHarnessApiKeyModal();
    });

    await waitFor(() => {
      expect(vi.mocked(selectProviderHarnessSource)).toHaveBeenNthCalledWith(1, "codex", "endpoint", "ep-new");
      expect(vi.mocked(selectProviderHarnessSource)).toHaveBeenNthCalledWith(2, "codex", "subscription", null);
      expect(controller?.providerError).toBe("bad endpoint");
    });
  });

  it("refreshes provider slices after Amp account mutations", async () => {
    let controller: Controller | null = null;
    const ampRow: HarnessAuthRow = {
      key: "amp:amp-2",
      kind: "subscription",
      label: "Amp Two",
      active: false,
      selectable: true,
      account_id: "amp-2",
    };

    vi.mocked(deleteAmpAccount).mockResolvedValue({
      active_account_id: "amp-2",
      accounts: [baseAmpAccounts.accounts[1]!],
    });
    vi.mocked(setAmpActiveAccount).mockResolvedValue({
      active_account_id: "amp-2",
      accounts: baseAmpAccounts.accounts,
    });
    queueBootstrapRefresh("ws-test", makeBootstrap({
      providers: [
        {
          provider_id: "codex",
          display_name: "Codex",
          installed: true,
          health: "ok",
          details: {
            install_target: "container",
          },
        } as never,
      ],
    }));

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      await controller?.onAmpDelete("amp-1");
    });
    await waitFor(() => {
      expect(vi.mocked(refreshProvidersBootstrapForScope)).toHaveBeenCalledTimes(1);
      expect(vi.mocked(invalidateProvidersBootstrap)).toHaveBeenCalledTimes(1);
      expect(requireController(controller).providers[0]?.details?.install_target).toBe("container");
    });
    const refreshCallsAfterDelete = vi.mocked(refreshProvidersBootstrapForScope).mock.calls.length;
    const invalidateCallsAfterDelete = vi.mocked(invalidateProvidersBootstrap).mock.calls.length;

    await act(async () => {
      await controller?.onSelectHarnessAuthRow("amp", ampRow);
    });
    await waitFor(() => {
      expect(vi.mocked(setAmpActiveAccount)).toHaveBeenCalledWith("amp-2");
      expect(vi.mocked(refreshProvidersBootstrapForScope).mock.calls.length).toBeGreaterThan(refreshCallsAfterDelete);
      expect(vi.mocked(invalidateProvidersBootstrap).mock.calls.length).toBeGreaterThan(invalidateCallsAfterDelete);
      expect(requireController(controller).providers[0]?.details?.install_target).toBe("container");
    });
  });

  it("warms Codex provider options after a workspace-scoped auth change", async () => {
    let controller: Controller | null = null;
    const codexRow: HarnessAuthRow = {
      key: "codex:acct-next",
      kind: "subscription",
      label: "Codex Next",
      active: false,
      selectable: true,
      account_id: "acct-next",
    };
    const pinnedCodexOptions = {
      provider_id: "codex",
      workspace_id: "ws-test",
      supports_load: false,
      auth_required: false,
      has_active_auth: true,
      auth_mode: "subscription" as const,
      source: {
        provider_id: "codex",
        selected_source_kind: "subscription" as const,
        selected_endpoint_id: null,
        endpoints: [],
      },
      probed_at: "2026-03-10T00:00:00.000Z",
      models: {
        models: [{ id: "gpt-5.3-codex/low" }, { id: "gpt-5.3-codex/medium" }],
        current_model_id: "gpt-5.3-codex/medium",
        meta: {
          source_kind: "subscription",
          catalog_source: "codex_bundle_pinned",
          refresh_pending: true,
        },
      },
    };

    setBootstrapSnapshot("ws-test", makeBootstrap({
      providers: [
        {
          provider_id: "codex",
          display_name: "Codex",
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
        } as never,
      ],
      provider_options: {
        codex: pinnedCodexOptions,
      },
      codex_accounts: {
        active_account_id: "acct-current",
        accounts: [
          { id: "acct-current", label: "Current", created_at: "2026-03-10T00:00:00.000Z" },
          { id: "acct-next", label: "Next", created_at: "2026-03-10T00:00:00.000Z" },
        ],
        logins: [],
      },
    }));
    queueBootstrapRefresh("ws-test", makeBootstrap({
      providers: [
        {
          provider_id: "codex",
          display_name: "Codex",
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
        } as never,
      ],
      provider_options: {
        codex: pinnedCodexOptions,
      },
      codex_accounts: {
        active_account_id: "acct-next",
        accounts: [
          { id: "acct-current", label: "Current", created_at: "2026-03-10T00:00:00.000Z" },
          { id: "acct-next", label: "Next", created_at: "2026-03-10T00:00:00.000Z" },
        ],
        logins: [],
      },
    }));
    vi.mocked(setCodexActiveAccount).mockResolvedValue({
      active_account_id: "acct-next",
      accounts: [
        { id: "acct-current", label: "Current", created_at: "2026-03-10T00:00:00.000Z" },
        { id: "acct-next", label: "Next", created_at: "2026-03-10T00:00:00.000Z" },
      ],
      logins: [],
    });
    vi.mocked(getProviderOptions).mockResolvedValue({
      ...pinnedCodexOptions,
      models: {
        models: [{ id: "gpt-5.4/low" }, { id: "gpt-5.4/medium" }],
        current_model_id: "gpt-5.4/medium",
      },
    });

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      await controller?.onSelectHarnessAuthRow("codex", codexRow);
    });

    await waitFor(() => {
      expect(vi.mocked(setCodexActiveAccount)).toHaveBeenCalledWith("acct-next");
      expect(vi.mocked(getProviderOptions)).toHaveBeenCalledWith("ws-test", "codex");
    });
  });

  it("preserves workspace-scoped providers when workspace refresh fails after a mutation", async () => {
    let controller: Controller | null = null;
    const workspaceProvider: ProviderStatus = {
      provider_id: "codex",
      installed: true,
      health: "ok",
      diagnostics: [],
      details: {
        install_target: "container",
      },
      usability: {
        usable: true,
        status: "ready",
        blocking_provider_ids: [],
        recommended_action: "none",
      },
    };

    setBootstrapSnapshot("ws-test", makeBootstrap({
      providers: [workspaceProvider],
    }));
    queueBootstrapRefresh("ws-test", new Error("workspace refresh failed"));
    vi.mocked(deleteAmpAccount).mockResolvedValue({
      active_account_id: "amp-2",
      accounts: [baseAmpAccounts.accounts[1]!],
    });

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
      expect(requireController(controller).providers[0]?.details?.install_target).toBe("container");
    });

    await act(async () => {
      await controller?.onAmpDelete("amp-1");
    });

    await waitFor(() => {
      expect(requireController(controller).providerError).toBe("workspace refresh failed");
    });

    expect(requireController(controller).providers[0]?.details?.install_target).toBe("container");
    expect(vi.mocked(loadHostProvidersBootstrap)).not.toHaveBeenCalled();
    expect(vi.mocked(refreshHostProvidersBootstrap)).not.toHaveBeenCalled();
    expect(vi.mocked(refreshProvidersBootstrapForScope)).toHaveBeenCalledWith(expect.objectContaining({
      kind: "workspace",
      workspaceId: "ws-test",
    }));
    expect(vi.mocked(invalidateHostProvidersBootstrap)).not.toHaveBeenCalled();
    expect(vi.mocked(invalidateProvidersBootstrap)).toHaveBeenCalledWith("ws-test");
  });

  it("polls pending workspace Codex logins via account endpoints without forcing bootstrap refresh", async () => {
    let controller: Controller | null = null;
    setBootstrapSnapshot("ws-test", makeBootstrap({
      providers: [
        {
          provider_id: "codex",
          display_name: "Codex",
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
        } as never,
      ],
      provider_options: {
        codex: {
          provider_id: "codex",
          workspace_id: "ws-test",
          supports_load: false,
          auth_required: true,
          has_active_auth: false,
          auth_mode: "subscription",
          source: {
            provider_id: "codex",
            selected_source_kind: "subscription",
            selected_endpoint_id: null,
            endpoints: [],
          },
          probed_at: "2026-03-10T00:00:00.000Z",
        },
      },
      codex_accounts: {
        active_account_id: null,
        accounts: [],
        logins: [
          {
            account_id: "codex-login-1",
            auth_url: "https://example.com/codex-login",
            status: "pending",
          },
        ],
      },
    }));
    vi.mocked(listCodexAccounts).mockResolvedValue({
      active_account_id: "acct-codex",
      accounts: [
        {
          id: "acct-codex",
          label: "Codex",
          created_at: "2026-03-11T00:00:00.000Z",
        },
      ],
      logins: [],
    });
    vi.mocked(getProviderOptions).mockResolvedValue({
      provider_id: "codex",
      workspace_id: "ws-test",
      supports_load: false,
      auth_required: false,
      has_active_auth: true,
      auth_mode: "subscription",
      source: {
        provider_id: "codex",
        selected_source_kind: "subscription",
        selected_endpoint_id: null,
        endpoints: [],
      },
      probed_at: "2026-03-11T00:00:01.000Z",
    });

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await waitFor(() => {
      expect(vi.mocked(listCodexAccounts)).toHaveBeenCalledTimes(1);
    }, { timeout: 4000 });
    await waitFor(() => {
      expect(vi.mocked(getProviderOptions)).toHaveBeenCalledWith("ws-test", "codex");
    }, { timeout: 4000 });

    expect(vi.mocked(refreshProvidersBootstrap)).not.toHaveBeenCalled();
    expect(requireController(controller).codexAccounts?.active_account_id).toBe("acct-codex");
    expect(requireController(controller).providerError).toBeNull();
  }, 10000);

  it("shares host-scoped mutations through the host bootstrap store without touching workspace scope", async () => {
    let firstController: Controller | null = null;
    let secondController: Controller | null = null;
    const hostProvider: ProviderStatus = {
      provider_id: "codex",
      installed: true,
      health: "ok",
      diagnostics: [],
      details: {
        install_target: "host",
      },
      usability: {
        usable: true,
        status: "ready",
        blocking_provider_ids: [],
        recommended_action: "none",
      },
    };

    setHostBootstrapSnapshot(makeBootstrap({
      providers: [hostProvider],
    }));
    vi.mocked(deleteAmpAccount).mockResolvedValue({
      active_account_id: "amp-2",
      accounts: [baseAmpAccounts.accounts[1]!],
    });

    render(createElement(Fragment, {}, [
      createElement(ControllerHarness, {
        key: "first",
        workspaceId: null,
        onChange: (next) => {
          firstController = next;
        },
      }),
      createElement(ControllerHarness, {
        key: "second",
        workspaceId: null,
        onChange: (next) => {
          secondController = next;
        },
      }),
    ]));

    await waitFor(() => {
      expect(firstController).not.toBeNull();
      expect(secondController).not.toBeNull();
      expect(requireController(firstController).providers[0]?.details?.install_target).toBe("host");
      expect(requireController(secondController).ampAccounts?.active_account_id).toBe("amp-1");
    });

    await act(async () => {
      await firstController?.onAmpDelete("amp-1");
    });

    await waitFor(() => {
      expect(requireController(firstController).ampAccounts?.active_account_id).toBe("amp-2");
      expect(requireController(secondController).ampAccounts?.active_account_id).toBe("amp-2");
    });

    expect(vi.mocked(invalidateHostProvidersBootstrap)).toHaveBeenCalledTimes(1);
    expect(vi.mocked(refreshProvidersBootstrapForScope)).toHaveBeenCalledWith(expect.objectContaining({
      kind: "host",
    }));
    expect(vi.mocked(refreshHostProvidersBootstrap)).not.toHaveBeenCalled();
    expect(vi.mocked(invalidateProvidersBootstrap)).not.toHaveBeenCalled();
    expect(vi.mocked(refreshProvidersBootstrap)).not.toHaveBeenCalled();
  });

  it("cancels an in-flight codex subscription poll when the modal closes", async () => {
    let controller: Controller | null = null;
    const loginPoll = deferred<{ status: "success" }>();

    vi.mocked(startCodexLogin).mockResolvedValue({
      account_id: "codex-login-1",
      auth_url: "https://example.com/codex-login",
      expected_callback_url: null,
      completion_token: "",
    });
    vi.mocked(getCodexLogin).mockReturnValue(loginPoll.promise as ReturnType<typeof getCodexLogin>);
    vi.mocked(openExternalLink).mockResolvedValue(true);

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      controller?.openHarnessAuthModal("codex");
    });
    await act(async () => {
      controller?.patchHarnessAuthModal({ stage: "subscription" });
    });

    let submitPromise: Promise<void> | undefined;
    await act(async () => {
      submitPromise = controller?.submitHarnessSubscriptionModal();
    });

    await waitFor(() => {
      expect(vi.mocked(startCodexLogin)).toHaveBeenCalledTimes(1);
      expect(vi.mocked(openExternalLink)).toHaveBeenCalledWith("https://example.com/codex-login");
    });

    await act(async () => {
      controller?.closeHarnessAuthModal();
    });

    loginPoll.resolve({ status: "success" });

    await act(async () => {
      await submitPromise;
    });

    expect(requireController(controller).harnessAuthModal).toBeNull();
    expect(vi.mocked(selectProviderHarnessSource)).not.toHaveBeenCalled();
    expect(requireController(controller).providerError).toBeNull();
  });

  it("does not open remote Codex OAuth when the desktop callback relay fails to start", async () => {
    let controller: Controller | null = null;

    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(desktopGetConnection).mockResolvedValue({
      kind: "ssh",
      host: "devbox.example",
      remote_port: 4399,
    } as Awaited<ReturnType<typeof desktopGetConnection>>);
    vi.mocked(desktopStartCodexLoginRelay).mockResolvedValue(false);
    vi.mocked(startCodexLogin).mockResolvedValue({
      account_id: "codex-login-remote",
      auth_url: "https://example.com/codex-login",
      expected_callback_url: "http://localhost:1455/auth/callback",
      completion_token: "completion-token",
    });

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      controller?.openHarnessAuthModal("codex");
      controller?.patchHarnessAuthModal({ stage: "subscription" });
    });

    await act(async () => {
      await controller?.submitHarnessSubscriptionModal();
    });

    expect(vi.mocked(desktopStartCodexLoginRelay)).toHaveBeenCalledWith({
      login_id: "codex-login-remote",
      callback_url: "http://localhost:1455/auth/callback",
      completion_token: "completion-token",
    });
    expect(vi.mocked(openExternalLink)).not.toHaveBeenCalled();
    expect(requireController(controller).providerError).toContain("remote callback relay");
  });

  it("suppresses stale subscription completion after switching providers", async () => {
    let controller: Controller | null = null;
    const loginPoll = deferred<{
      login_id: string;
      auth_url?: string | null;
      status: string;
      error?: string | null;
    }>();

    vi.mocked(startGeminiLogin).mockResolvedValue({
      login_id: "gemini-login-1",
      auth_url: "https://example.com/gemini-login",
    });
    vi.mocked(getGeminiLogin).mockReturnValue(loginPoll.promise as ReturnType<typeof getGeminiLogin>);
    vi.mocked(openExternalLink).mockResolvedValue(true);

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      controller?.openHarnessAuthModal("gemini");
    });

    let submitPromise: Promise<void> | undefined;
    await act(async () => {
      submitPromise = controller?.submitHarnessSubscriptionModal();
    });

    await waitFor(() => {
      expect(vi.mocked(startGeminiLogin)).toHaveBeenCalledTimes(1);
      expect(vi.mocked(openExternalLink)).toHaveBeenCalledWith("https://example.com/gemini-login");
    });

    await act(async () => {
      controller?.openHarnessAuthModal("qwen");
    });

    loginPoll.resolve({
      login_id: "gemini-login-1",
      auth_url: "https://example.com/gemini-login",
      status: "success",
    });

    await act(async () => {
      await submitPromise;
    });

    expect(requireController(controller).harnessAuthModal?.provider_id).toBe("qwen");
    expect(requireController(controller).harnessAuthModal?.subscription_busy).toBe(false);
    expect(vi.mocked(selectProviderHarnessSource)).not.toHaveBeenCalled();
    expect(requireController(controller).providerError).toBeNull();
  });

  it("starts Cursor browser sign-in and closes the modal on success", async () => {
    let controller: Controller | null = null;

    vi.mocked(startCursorLogin).mockResolvedValue({
      login_id: "cursor-login-1",
      auth_url: "https://cursor.com/login/device?code=test",
    });
    vi.mocked(getCursorLogin).mockResolvedValue({
      login_id: "cursor-login-1",
      auth_url: "https://cursor.com/login/device?code=test",
      status: "success",
      account_id: "cursor-acct-1",
    });
    vi.mocked(openExternalLink).mockResolvedValue(true);
    queueBootstrapRefresh(
      "ws-test",
      makeBootstrap({
        cursor_accounts: {
          active_account_id: "cursor-acct-1",
          accounts: [
            {
              id: "cursor-acct-1",
              label: "Cursor OAuth",
              kind: "oauth-token",
              email: "cursor@example.com",
              created_at: "2026-03-11T00:00:00Z",
            },
          ],
        },
      }),
      makeBootstrap({
        cursor_accounts: {
          active_account_id: "cursor-acct-1",
          accounts: [
            {
              id: "cursor-acct-1",
              label: "Cursor OAuth",
              kind: "oauth-token",
              email: "cursor@example.com",
              created_at: "2026-03-11T00:00:00Z",
            },
          ],
        },
      }),
    );

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      controller?.openHarnessAuthModal("cursor");
      controller?.patchHarnessAuthModal({ stage: "subscription" });
    });

    await act(async () => {
      await controller?.submitHarnessSubscriptionModal();
    });

    await waitFor(() => {
      expect(vi.mocked(startCursorLogin)).toHaveBeenCalledTimes(1);
      expect(vi.mocked(openExternalLink)).toHaveBeenCalledWith(
        "https://cursor.com/login/device?code=test",
      );
    });

    expect(requireController(controller).harnessAuthModal).toBeNull();
    expect(requireController(controller).providerError).toBeNull();
    expect(vi.mocked(selectProviderHarnessSource)).not.toHaveBeenCalled();
  });

  it("moves browser-auth status to finalizing while reconciliation is pending", async () => {
    let controller: Controller | null = null;
    const deferredRefresh = deferred<ProvidersBootstrapResponse>();
    const refreshedBootstrap = makeBootstrap({
      cursor_accounts: {
        active_account_id: "cursor-acct-1",
        accounts: [
          {
            id: "cursor-acct-1",
            label: "Cursor OAuth",
            kind: "oauth-token",
            email: "cursor@example.com",
            created_at: "2026-03-11T00:00:00Z",
          },
        ],
      },
    });

    vi.mocked(startCursorLogin).mockResolvedValue({
      login_id: "cursor-login-1",
      auth_url: "https://cursor.com/login/device?code=test",
    });
    vi.mocked(getCursorLogin).mockResolvedValue({
      login_id: "cursor-login-1",
      auth_url: "https://cursor.com/login/device?code=test",
      status: "success",
      account_id: "cursor-acct-1",
    });
    vi.mocked(openExternalLink).mockResolvedValue(true);
    vi.mocked(refreshProvidersBootstrap)
      .mockImplementationOnce(async () => deferredRefresh.promise)
      .mockImplementationOnce(async () => setBootstrapSnapshot("ws-test", refreshedBootstrap));

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      controller?.openHarnessAuthModal("cursor");
      controller?.patchHarnessAuthModal({
        stage: "subscription",
        subscription_label: "Cursor Login",
      });
    });

    let submitPromise: Promise<void> | undefined;
    await act(async () => {
      submitPromise = controller?.submitHarnessSubscriptionModal();
    });

    await waitFor(() => {
      expect(vi.mocked(startCursorLogin)).toHaveBeenCalledTimes(1);
    });

    await waitFor(() => {
      expect(requireController(controller).harnessAuthModal?.subscription_phase).toBe("finalizing");
      expect(requireController(controller).harnessAuthModal?.subscription_status).toBe("Finalizing sign-in...");
    });

    deferredRefresh.resolve(refreshedBootstrap);

    await act(async () => {
      await submitPromise;
    });

    expect(requireController(controller).harnessAuthModal).toBeNull();
    expect(requireController(controller).providerError).toBeNull();
  });

  it("starts Kimi sign-in by opening the browser and surfaces auth state while polling", async () => {
    let controller: Controller | null = null;
    const loginPoll = deferred<{
      login_id: string;
      auth_url?: string | null;
      device_code?: string | null;
      status: string;
      error?: string | null;
    }>();

    vi.mocked(startKimiLogin).mockResolvedValue({
      login_id: "kimi-login-1",
      auth_url: "https://kimi.example.com/login/device",
    });
    vi.mocked(openExternalLink).mockResolvedValue(true);
    vi.mocked(getKimiLogin).mockReturnValue(loginPoll.promise as ReturnType<typeof getKimiLogin>);
    queueBootstrapRefresh(
      "ws-test",
      makeBootstrap({
        kimi_accounts: {
          active_account_id: "kimi-acct-1",
          accounts: [
            {
              id: "kimi-acct-1",
              label: "Kimi OAuth",
              email: "kimi@example.com",
              created_at: "2026-03-11T00:00:00Z",
            },
          ],
        },
      }),
    );

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      controller?.openHarnessAuthModal("kimi");
      controller?.patchHarnessAuthModal({
        stage: "subscription",
        subscription_label: "Kimi Login",
      });
    });

    let submitPromise: Promise<void> | undefined;
    await act(async () => {
      submitPromise = controller?.submitHarnessSubscriptionModal();
    });

    await waitFor(() => {
      expect(vi.mocked(startKimiLogin)).toHaveBeenCalledWith("Kimi Login");
    });

    expect(vi.mocked(openExternalLink)).toHaveBeenCalledWith(
      "https://kimi.example.com/login/device",
    );
    expect(requireController(controller).harnessAuthModal?.subscription_auth_url).toBe(
      "https://kimi.example.com/login/device",
    );

    loginPoll.resolve({
      login_id: "kimi-login-1",
      auth_url: "https://kimi.example.com/login/device",
      device_code: "KIMI-1234",
      status: "success",
    });

    await act(async () => {
      await submitPromise;
    });

    expect(requireController(controller).harnessAuthModal).toBeNull();
    expect(requireController(controller).providerError).toBeNull();
    expect(vi.mocked(selectProviderHarnessSource)).toHaveBeenCalledWith("kimi", "subscription", null);
    expect(analyticsMocks.trackProviderAuthStarted).toHaveBeenCalledWith({
      providerId: "kimi",
      authMethod: "subscription_browser",
    });
    expect(analyticsMocks.trackProviderAuthCompleted).toHaveBeenCalledWith({
      providerId: "kimi",
      authMethod: "subscription_browser",
    });
    expect(analyticsMocks.trackProviderAuthFailed).not.toHaveBeenCalled();
  });

  it("starts Claude setup-token sign-in without any web-driven browser open", async () => {
    let controller: Controller | null = null;
    const authUrl = "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A58215%2Fcallback";

    vi.mocked(startClaudeLogin).mockResolvedValue({
      login_id: "claude-login-1",
      auth_url: authUrl,
    });
    vi.mocked(getClaudeLogin)
      .mockResolvedValueOnce({
        login_id: "claude-login-1",
        auth_url: authUrl,
        status: "pending",
      } as Awaited<ReturnType<typeof getClaudeLogin>>)
      .mockResolvedValueOnce({
        login_id: "claude-login-1",
        auth_url: authUrl,
        status: "success",
      } as Awaited<ReturnType<typeof getClaudeLogin>>);
    queueBootstrapRefresh(
      "ws-test",
      makeBootstrap({
        claude_accounts: {
          active_account_id: "claude-acct-1",
          accounts: [
            {
              id: "claude-acct-1",
              label: "Claude setup token",
              kind: "setup_token",
              created_at: "2026-03-11T00:00:00Z",
            },
          ],
        },
      }),
    );

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      controller?.openHarnessAuthModal("claude-crp");
      controller?.patchHarnessAuthModal({
        stage: "subscription",
        subscription_label: "Claude Login",
      });
    });

    let submitPromise: Promise<void> | undefined;
    await act(async () => {
      submitPromise = controller?.submitHarnessSubscriptionModal();
    });

    await waitFor(() => {
      expect(vi.mocked(startClaudeLogin)).toHaveBeenCalledWith("Claude Login");
    });

    expect(vi.mocked(openExternalLink)).not.toHaveBeenCalled();
    expect(requireController(controller).harnessAuthModal?.subscription_status).toBe(
      "Waiting for Claude setup-token sign-in to complete in your browser...",
    );
    expect(requireController(controller).harnessAuthModal?.subscription_auth_url).toBe(authUrl);
    expect(vi.mocked(openExternalLink)).not.toHaveBeenCalled();

    await act(async () => {
      await submitPromise;
    });

    expect(requireController(controller).harnessAuthModal).toBeNull();
    expect(requireController(controller).providerError).toBeNull();
    expect(vi.mocked(openExternalLink)).not.toHaveBeenCalled();
    expect(vi.mocked(selectProviderHarnessSource)).toHaveBeenCalledWith("claude-crp", "subscription", null);
  });

  it("fails Kimi sign-in instead of exposing a manual browser fallback when browser launch fails", async () => {
    let controller: Controller | null = null;
    const authUrl = "https://kimi.example.com/login/device";

    vi.mocked(openExternalLink).mockResolvedValue(false);
    vi.mocked(startKimiLogin).mockResolvedValue({
      login_id: "kimi-login-2",
      auth_url: authUrl,
      device_code: "KIMI-1234",
    });

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      controller?.openHarnessAuthModal("kimi");
      controller?.patchHarnessAuthModal({
        stage: "subscription",
        subscription_label: "Kimi Login",
      });
    });

    await act(async () => {
      await controller?.submitHarnessSubscriptionModal();
    });

    expect(vi.mocked(startKimiLogin)).toHaveBeenCalledWith("Kimi Login");
    expect(vi.mocked(openExternalLink)).toHaveBeenCalledWith(authUrl);
    expect(vi.mocked(getKimiLogin)).not.toHaveBeenCalled();
    expect(requireController(controller).harnessAuthModal?.subscription_auth_url).toBe(authUrl);
    expect(requireController(controller).harnessAuthModal?.subscription_status).toBe(
      "Subscription flow failed. Check error details below.",
    );
    expect(requireController(controller).providerError).toBe(
      "Failed to launch the sign-in browser window.",
    );
  });

  it("starts Amp sign-in without auto-opening the browser and surfaces auth state while polling", async () => {
    let controller: Controller | null = null;
    const loginPoll = deferred<{
      login_id: string;
      auth_url?: string | null;
      device_code?: string | null;
      status: string;
      error?: string | null;
    }>();

    vi.mocked(startAmpLogin).mockResolvedValue({
      login_id: "amp-login-1",
      auth_url: "https://ampcode.com/auth/cli-login?authToken=test&callbackPort=35789",
    });
    vi.mocked(getAmpLogin).mockReturnValue(loginPoll.promise as ReturnType<typeof getAmpLogin>);
    queueBootstrapRefresh(
      "ws-test",
      makeBootstrap({
        amp_accounts: {
          active_account_id: "amp-acct-1",
          accounts: [
            {
              id: "amp-acct-1",
              label: "Amp OAuth",
              email: "amp@example.com",
              created_at: "2026-03-11T00:00:00Z",
            },
          ],
        },
      }),
    );

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      controller?.openHarnessAuthModal("amp");
      controller?.patchHarnessAuthModal({
        stage: "subscription",
        subscription_label: "Amp Login",
      });
    });

    let submitPromise: Promise<void> | undefined;
    await act(async () => {
      submitPromise = controller?.submitHarnessSubscriptionModal();
    });

    await waitFor(() => {
      expect(vi.mocked(startAmpLogin)).toHaveBeenCalledWith("Amp Login");
    });

    expect(vi.mocked(openExternalLink)).not.toHaveBeenCalled();
    expect(requireController(controller).harnessAuthModal?.subscription_status).toBe(
      "Waiting for Amp sign-in to complete in your browser...",
    );
    expect(requireController(controller).harnessAuthModal?.subscription_auth_url).toBe(
      "https://ampcode.com/auth/cli-login?authToken=test&callbackPort=35789",
    );

    loginPoll.resolve({
      login_id: "amp-login-1",
      auth_url: "https://ampcode.com/auth/cli-login?authToken=test&callbackPort=35789",
      status: "success",
    });

    await act(async () => {
      await submitPromise;
    });

    expect(requireController(controller).harnessAuthModal).toBeNull();
    expect(requireController(controller).providerError).toBeNull();
    expect(vi.mocked(selectProviderHarnessSource)).not.toHaveBeenCalled();
  });

  it("suppresses stale api-key submit effects after switching providers", async () => {
    let controller: Controller | null = null;
    const endpointUpsert = deferred<HarnessProviderSourceConfig>();

    vi.mocked(upsertProviderHarnessEndpoint).mockReturnValue(
      endpointUpsert.promise as ReturnType<typeof upsertProviderHarnessEndpoint>,
    );

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      controller?.openHarnessAuthModal("codex");
    });
    await act(async () => {
      controller?.patchHarnessAuthModal({
        stage: "api_key",
        api_key: "sk-test",
      });
    });

    let submitPromise: Promise<void> | undefined;
    await act(async () => {
      submitPromise = controller?.submitHarnessApiKeyModal();
    });

    await waitFor(() => {
      expect(vi.mocked(upsertProviderHarnessEndpoint)).toHaveBeenCalledTimes(1);
    });

    await act(async () => {
      controller?.openHarnessAuthModal("gemini");
    });

    endpointUpsert.resolve({
      ...baseCodexConfig,
      endpoints: [
        ...baseCodexConfig.endpoints,
        {
          ...baseEndpoint,
          id: "ep-new",
          name: "Secondary",
          updated_at: "2026-03-03T00:00:00Z",
        },
      ],
    });

    await act(async () => {
      await submitPromise;
    });

    expect(requireController(controller).harnessAuthModal?.provider_id).toBe("gemini");
    expect(vi.mocked(selectProviderHarnessSource)).not.toHaveBeenCalled();
    expect(vi.mocked(verifyProviderForWorkspace)).not.toHaveBeenCalled();
    expect(requireController(controller).providerError).toBeNull();
  });

  it("does not let a stale codex poll close a reopened codex modal", async () => {
    let controller: Controller | null = null;
    const loginPoll = deferred<{ status: "success" }>();

    vi.mocked(startCodexLogin).mockResolvedValue({
      account_id: "codex-login-2",
      auth_url: "https://example.com/codex-login-2",
      expected_callback_url: null,
      completion_token: "",
    });
    vi.mocked(getCodexLogin).mockReturnValue(loginPoll.promise as ReturnType<typeof getCodexLogin>);
    vi.mocked(openExternalLink).mockResolvedValue(true);

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      controller?.openHarnessAuthModal("codex");
      controller?.patchHarnessAuthModal({ stage: "subscription" });
    });

    let submitPromise: Promise<void> | undefined;
    await act(async () => {
      submitPromise = controller?.submitHarnessSubscriptionModal();
    });

    await waitFor(() => {
      expect(vi.mocked(startCodexLogin)).toHaveBeenCalledTimes(1);
    });

    await act(async () => {
      controller?.closeHarnessAuthModal();
      controller?.openHarnessAuthModal("codex");
    });

    loginPoll.resolve({ status: "success" });

    await act(async () => {
      await submitPromise;
    });

    expect(requireController(controller).harnessAuthModal?.provider_id).toBe("codex");
    expect(requireController(controller).harnessAuthModal).not.toBeNull();
    expect(vi.mocked(selectProviderHarnessSource)).not.toHaveBeenCalled();
    expect(requireController(controller).providerError).toBeNull();
  });

  it("rolls back endpoint selection silently when verification finishes after switching providers", async () => {
    let controller: Controller | null = null;
    const verifyDeferred = deferred<ProviderAuthCheck>();
    const freshEndpoint = {
      ...baseEndpoint,
      id: "ep-switched",
      name: "Rollback Test",
      updated_at: "2026-03-04T00:00:00Z",
    };
    const afterUpsert: HarnessProviderSourceConfig = {
      ...baseCodexConfig,
      endpoints: [...baseCodexConfig.endpoints, freshEndpoint],
    };
    const selectedEndpointConfig: HarnessProviderSourceConfig = {
      ...afterUpsert,
      selected_source_kind: "endpoint",
      selected_endpoint_id: freshEndpoint.id,
    };

    vi.mocked(upsertProviderHarnessEndpoint).mockResolvedValue(afterUpsert);
    vi.mocked(selectProviderHarnessSource)
      .mockResolvedValueOnce(selectedEndpointConfig)
      .mockResolvedValueOnce(baseCodexConfig);
    queueBootstrapRefresh(
      "ws-test",
      makeBootstrap({
        provider_harness_config: {
          codex: selectedEndpointConfig,
        },
      }),
      makeBootstrap(),
    );
    vi.mocked(verifyProviderForWorkspace).mockReturnValue(
      verifyDeferred.promise as ReturnType<typeof verifyProviderForWorkspace>,
    );

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      controller?.openHarnessAuthModal("codex");
      controller?.patchHarnessAuthModal({
        stage: "api_key",
        api_key: "sk-test",
      });
    });

    let submitPromise: Promise<void> | undefined;
    await act(async () => {
      submitPromise = controller?.submitHarnessApiKeyModal();
    });

    await waitFor(() => {
      expect(vi.mocked(selectProviderHarnessSource)).toHaveBeenNthCalledWith(1, "codex", "endpoint", "ep-switched");
      expect(vi.mocked(verifyProviderForWorkspace)).toHaveBeenCalledWith("ws-test", "codex");
    });

    await act(async () => {
      controller?.openHarnessAuthModal("gemini");
    });

    verifyDeferred.resolve({
      provider_id: "codex",
      workspace_id: "ws-test",
      status: "failed",
      message: "bad endpoint",
    });

    await act(async () => {
      await submitPromise;
    });

    expect(vi.mocked(selectProviderHarnessSource)).toHaveBeenNthCalledWith(2, "codex", "subscription", null);
    expect(requireController(controller).harnessAuthModal?.provider_id).toBe("gemini");
    expect(requireController(controller).providerError).toBeNull();
  });

  it("suppresses duplicate subscription starts before busy state flushes", async () => {
    let controller: Controller | null = null;
    const startAmpLoginDeferred = deferred<{ login_id: string; auth_url?: string | null }>();
    vi.mocked(startAmpLogin).mockReturnValue(
      startAmpLoginDeferred.promise as ReturnType<typeof startAmpLogin>,
    );

    render(createElement(ControllerHarness, {
      onChange: (next) => {
        controller = next;
      },
    }));

    await waitFor(() => {
      expect(controller).not.toBeNull();
    });

    await act(async () => {
      controller?.openHarnessAuthModal("amp");
      controller?.patchHarnessAuthModal({
        stage: "subscription",
        subscription_label: "Amp Login",
      });
    });

    await act(async () => {
      void controller?.submitHarnessSubscriptionModal();
      void controller?.submitHarnessSubscriptionModal();
    });

    expect(vi.mocked(startAmpLogin)).toHaveBeenCalledTimes(1);

    await act(async () => {
      startAmpLoginDeferred.resolve({
        login_id: "amp-login-1",
        auth_url: "https://example.com/amp-login",
      });
      await Promise.resolve();
    });
  });
});
