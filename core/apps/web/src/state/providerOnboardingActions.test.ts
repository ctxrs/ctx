import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  HarnessProviderSourceConfig,
  ProvidersBootstrapResponse,
} from "../api/client";

const clientMocks = vi.hoisted(() => ({
  deleteAmpAccount: vi.fn(),
  deleteClaudeAccount: vi.fn(),
  deleteCodexAccount: vi.fn(),
  deleteCopilotAccount: vi.fn(),
  deleteCursorAccount: vi.fn(),
  deleteGeminiAccount: vi.fn(),
  deleteKimiAccount: vi.fn(),
  deleteMistralAccount: vi.fn(),
  deleteProviderHarnessEndpoint: vi.fn(),
  deleteQwenAccount: vi.fn(),
  getProviderHarnessConfig: vi.fn(),
  getProvidersBootstrap: vi.fn(),
  listAmpAccounts: vi.fn(),
  listClaudeAccounts: vi.fn(),
  listCodexAccounts: vi.fn(),
  listCopilotAccounts: vi.fn(),
  listCursorAccounts: vi.fn(),
  listGeminiAccounts: vi.fn(),
  listKimiAccounts: vi.fn(),
  listMistralAccounts: vi.fn(),
  listProviders: vi.fn(),
  listQwenAccounts: vi.fn(),
  refreshProviderHarnessEndpointModels: vi.fn(),
  selectProviderHarnessSource: vi.fn(),
  setAmpActiveAccount: vi.fn(),
  setClaudeActiveAccount: vi.fn(),
  setCodexActiveAccount: vi.fn(),
  setCopilotActiveAccount: vi.fn(),
  setCursorActiveAccount: vi.fn(),
  setGeminiActiveAccount: vi.fn(),
  setKimiActiveAccount: vi.fn(),
  setMistralActiveAccount: vi.fn(),
  setQwenActiveAccount: vi.fn(),
  upsertProviderHarnessEndpoint: vi.fn(),
  verifyProviderForWorkspace: vi.fn(),
}));

vi.mock("../api/client", () => clientMocks);

const analyticsMocks = vi.hoisted(() => ({
  trackProviderAuthCompleted: vi.fn(),
  trackProviderAuthFailed: vi.fn(),
  trackProviderAuthStarted: vi.fn(),
}));

vi.mock("../utils/analytics", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../utils/analytics")>();
  return {
    ...actual,
    ...analyticsMocks,
  };
});

const emptyAccounts = {
  active_account_id: null,
  accounts: [],
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

const makeProvider = (installTarget: "host" | "container") => ({
  provider_id: "codex",
  display_name: "Codex",
  installed: true,
  health: "ok",
  diagnostics: [],
  details: {
    install_target: installTarget,
  },
}) as never;

const makeWorkspaceBootstrap = (
  workspaceId: string,
  overrides?: Partial<ProvidersBootstrapResponse>,
): ProvidersBootstrapResponse => ({
  providers: [makeProvider("container")],
  provider_options: {
    codex: {
      provider_id: "codex",
      workspace_id: workspaceId,
      supports_load: false,
      auth_required: false,
      has_active_auth: true,
      auth_mode: "subscription" as const,
      account_identity: null,
      source: baseCodexConfig,
      probed_at: "2026-03-10T00:00:00.000Z",
    },
  },
  provider_harness_config: {
    codex: baseCodexConfig,
  },
  codex_accounts: {
    active_account_id: null,
    accounts: [],
    logins: [],
  },
  claude_accounts: emptyAccounts,
  gemini_accounts: emptyAccounts,
  qwen_accounts: emptyAccounts,
  kimi_accounts: emptyAccounts,
  mistral_accounts: emptyAccounts,
  copilot_accounts: emptyAccounts,
  cursor_accounts: emptyAccounts,
  amp_accounts: emptyAccounts,
  ...overrides,
});

const makeHostBootstrap = (
  overrides?: Partial<ProvidersBootstrapResponse>,
): ProvidersBootstrapResponse => ({
  providers: [makeProvider("host")],
  provider_options: {},
  provider_harness_config: {
    codex: baseCodexConfig,
  },
  codex_accounts: {
    active_account_id: null,
    accounts: [],
    logins: [],
  },
  claude_accounts: emptyAccounts,
  gemini_accounts: emptyAccounts,
  qwen_accounts: emptyAccounts,
  kimi_accounts: emptyAccounts,
  mistral_accounts: emptyAccounts,
  copilot_accounts: emptyAccounts,
  cursor_accounts: emptyAccounts,
  amp_accounts: emptyAccounts,
  ...overrides,
});

const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
};

let workspaceSnapshots: Map<string, ProvidersBootstrapResponse>;
let hostSnapshot: ProvidersBootstrapResponse;

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();

  workspaceSnapshots = new Map();
  hostSnapshot = makeHostBootstrap();

  clientMocks.deleteProviderHarnessEndpoint.mockReset();
  clientMocks.deleteAmpAccount.mockReset();
  clientMocks.deleteClaudeAccount.mockReset();
  clientMocks.deleteCodexAccount.mockReset();
  clientMocks.deleteCopilotAccount.mockReset();
  clientMocks.deleteCursorAccount.mockReset();
  clientMocks.deleteGeminiAccount.mockReset();
  clientMocks.deleteKimiAccount.mockReset();
  clientMocks.deleteMistralAccount.mockReset();
  clientMocks.deleteQwenAccount.mockReset();
  clientMocks.getProviderHarnessConfig.mockImplementation(async (providerId: string) =>
    hostSnapshot.provider_harness_config[providerId] ?? {
      provider_id: providerId,
      selected_source_kind: "subscription",
      selected_endpoint_id: null,
      endpoints: [],
    });
  clientMocks.getProvidersBootstrap.mockImplementation(async (workspaceId: string) =>
    workspaceSnapshots.get(workspaceId) ?? makeWorkspaceBootstrap(workspaceId));
  clientMocks.listAmpAccounts.mockImplementation(async () => hostSnapshot.amp_accounts);
  clientMocks.listClaudeAccounts.mockImplementation(async () => hostSnapshot.claude_accounts);
  clientMocks.listCodexAccounts.mockImplementation(async () => hostSnapshot.codex_accounts);
  clientMocks.listCopilotAccounts.mockImplementation(async () => hostSnapshot.copilot_accounts);
  clientMocks.listCursorAccounts.mockImplementation(async () => hostSnapshot.cursor_accounts);
  clientMocks.listGeminiAccounts.mockImplementation(async () => hostSnapshot.gemini_accounts);
  clientMocks.listKimiAccounts.mockImplementation(async () => hostSnapshot.kimi_accounts);
  clientMocks.listMistralAccounts.mockImplementation(async () => hostSnapshot.mistral_accounts);
  clientMocks.listProviders.mockImplementation(async () => hostSnapshot.providers);
  clientMocks.listQwenAccounts.mockImplementation(async () => hostSnapshot.qwen_accounts);
  clientMocks.refreshProviderHarnessEndpointModels.mockReset();
  clientMocks.selectProviderHarnessSource.mockReset();
  clientMocks.setAmpActiveAccount.mockReset();
  clientMocks.setClaudeActiveAccount.mockReset();
  clientMocks.setCodexActiveAccount.mockReset();
  clientMocks.setCopilotActiveAccount.mockReset();
  clientMocks.setCursorActiveAccount.mockReset();
  clientMocks.setGeminiActiveAccount.mockReset();
  clientMocks.setKimiActiveAccount.mockReset();
  clientMocks.setMistralActiveAccount.mockReset();
  clientMocks.setQwenActiveAccount.mockReset();
  clientMocks.upsertProviderHarnessEndpoint.mockReset();
  clientMocks.verifyProviderForWorkspace.mockReset();
  analyticsMocks.trackProviderAuthCompleted.mockReset();
  analyticsMocks.trackProviderAuthFailed.mockReset();
  analyticsMocks.trackProviderAuthStarted.mockReset();
});

describe("providerOnboardingActions", () => {
  it("deleteProviderAccount updates the workspace account slice and refreshes bootstrap", async () => {
    const workspaceId = "ws-delete-account";
    const nextAccounts = {
      active_account_id: "acct-next",
      accounts: [{
        id: "acct-next",
        label: "Account Next",
        created_at: "2026-03-10T00:00:00Z",
      }],
      logins: [],
    };
    workspaceSnapshots.set(workspaceId, makeWorkspaceBootstrap(workspaceId, {
      codex_accounts: nextAccounts,
    }));
    clientMocks.deleteCodexAccount.mockResolvedValue(nextAccounts);

    const daemonConnection = await import("../api/daemonConnection");
    daemonConnection.setDaemonConnection({
      baseUrl: "https://daemon-delete-account.example",
      source: "test",
    });
    const store = await import("./providersBootstrapStore");
    const actions = await import("./providerOnboardingActions");
    const scopeAdapters = await import("./providerScopeAdapters");

    store.updateProvidersBootstrap(workspaceId, () => makeWorkspaceBootstrap(workspaceId));

    await actions.deleteProviderAccount(
      scopeAdapters.getProviderOwnerScope(workspaceId),
      "codex",
      "acct-old",
    );

    expect(clientMocks.deleteCodexAccount).toHaveBeenCalledWith("acct-old");
    expect(clientMocks.getProvidersBootstrap).toHaveBeenCalledWith(workspaceId);
    expect(store.getProvidersBootstrapSnapshot(workspaceId).codex_accounts.active_account_id).toBe("acct-next");
    expect(store.getProvidersBootstrapSnapshot(workspaceId).providers[0]?.details?.install_target).toBe("container");
  });

  it("deleteProviderAccount updates the host account slice without touching workspace scope", async () => {
    const workspaceId = "ws-workspace-unchanged";
    const nextAccounts = {
      active_account_id: "acct-host-next",
      accounts: [{
        id: "acct-host-next",
        label: "Host Account Next",
        created_at: "2026-03-10T00:00:00Z",
      }],
      logins: [],
    };
    hostSnapshot = makeHostBootstrap({
      codex_accounts: nextAccounts,
    });
    clientMocks.deleteCodexAccount.mockResolvedValue(nextAccounts);

    const daemonConnection = await import("../api/daemonConnection");
    daemonConnection.setDaemonConnection({
      baseUrl: "https://daemon-host-delete-account.example",
      source: "test",
    });
    const store = await import("./providersBootstrapStore");
    const actions = await import("./providerOnboardingActions");
    const scopeAdapters = await import("./providerScopeAdapters");

    store.updateHostProvidersBootstrap(() => makeHostBootstrap());
    store.updateProvidersBootstrap(workspaceId, () => makeWorkspaceBootstrap(workspaceId));

    await actions.deleteProviderAccount(
      scopeAdapters.getProviderOwnerScope(null),
      "codex",
      "acct-host-old",
    );

    expect(clientMocks.deleteCodexAccount).toHaveBeenCalledWith("acct-host-old");
    expect(store.getHostProvidersBootstrapSnapshot().codex_accounts.active_account_id).toBe("acct-host-next");
    expect(store.getProvidersBootstrapSnapshot(workspaceId).codex_accounts.active_account_id).toBeNull();
  });

  it("deleteProviderEndpoint updates the workspace harness-config slice and refreshes bootstrap", async () => {
    const workspaceId = "ws-delete-endpoint";
    const nextConfig: HarnessProviderSourceConfig = {
      ...baseCodexConfig,
      endpoints: [],
    };
    workspaceSnapshots.set(workspaceId, makeWorkspaceBootstrap(workspaceId, {
      provider_harness_config: {
        codex: nextConfig,
      },
    }));
    clientMocks.deleteProviderHarnessEndpoint.mockResolvedValue(nextConfig);

    const daemonConnection = await import("../api/daemonConnection");
    daemonConnection.setDaemonConnection({
      baseUrl: "https://daemon-delete.example",
      source: "test",
    });
    const store = await import("./providersBootstrapStore");
    const actions = await import("./providerOnboardingActions");
    const scopeAdapters = await import("./providerScopeAdapters");

    store.updateProvidersBootstrap(workspaceId, () => makeWorkspaceBootstrap(workspaceId));

    await actions.deleteProviderEndpoint(
      scopeAdapters.getProviderOwnerScope(workspaceId),
      "codex",
      "ep-old",
    );

    expect(clientMocks.deleteProviderHarnessEndpoint).toHaveBeenCalledWith("codex", "ep-old");
    expect(clientMocks.getProvidersBootstrap).toHaveBeenCalledWith(workspaceId);
    expect(store.getProvidersBootstrapSnapshot(workspaceId).provider_harness_config.codex?.endpoints).toEqual([]);
    expect(store.getProvidersBootstrapSnapshot(workspaceId).providers[0]?.details?.install_target).toBe("container");
  });

  it("refreshProviderEndpointModels updates the workspace harness-config slice and refreshes bootstrap", async () => {
    const workspaceId = "ws-refresh-models";
    const nextConfig: HarnessProviderSourceConfig = {
      ...baseCodexConfig,
      endpoints: [
        {
          ...baseEndpoint,
          model_catalog_status: "ready",
          model_catalog_models: [{ id: "gpt-5" }],
          model_catalog_source: "remote",
        },
      ],
    };
    workspaceSnapshots.set(workspaceId, makeWorkspaceBootstrap(workspaceId, {
      provider_harness_config: {
        codex: nextConfig,
      },
    }));
    clientMocks.refreshProviderHarnessEndpointModels.mockResolvedValue(nextConfig);

    const daemonConnection = await import("../api/daemonConnection");
    daemonConnection.setDaemonConnection({
      baseUrl: "https://daemon-models.example",
      source: "test",
    });
    const store = await import("./providersBootstrapStore");
    const actions = await import("./providerOnboardingActions");
    const scopeAdapters = await import("./providerScopeAdapters");

    store.updateProvidersBootstrap(workspaceId, () => makeWorkspaceBootstrap(workspaceId));

    await actions.refreshProviderEndpointModels(
      scopeAdapters.getProviderOwnerScope(workspaceId),
      "codex",
      "ep-old",
    );

    expect(clientMocks.refreshProviderHarnessEndpointModels).toHaveBeenCalledWith("codex", "ep-old");
    expect(store.getProvidersBootstrapSnapshot(workspaceId).provider_harness_config.codex?.endpoints[0]?.model_catalog_status)
      .toBe("ready");
    expect(store.getProvidersBootstrapSnapshot(workspaceId).providers[0]?.details?.install_target).toBe("container");
  });

  it("selectProviderSource refreshes bootstrap after a successful source change", async () => {
    const workspaceId = "ws-select-source";
    const selectedConfig: HarnessProviderSourceConfig = {
      ...baseCodexConfig,
      selected_source_kind: "endpoint",
      selected_endpoint_id: "ep-old",
    };
    workspaceSnapshots.set(workspaceId, makeWorkspaceBootstrap(workspaceId, {
      provider_harness_config: {
        codex: selectedConfig,
      },
    }));
    clientMocks.selectProviderHarnessSource.mockResolvedValue(selectedConfig);

    const daemonConnection = await import("../api/daemonConnection");
    daemonConnection.setDaemonConnection({
      baseUrl: "https://daemon-select.example",
      source: "test",
    });
    const store = await import("./providersBootstrapStore");
    const actions = await import("./providerOnboardingActions");
    const scopeAdapters = await import("./providerScopeAdapters");

    store.updateProvidersBootstrap(workspaceId, () => makeWorkspaceBootstrap(workspaceId));

    await actions.selectProviderSource(
      scopeAdapters.getProviderOwnerScope(workspaceId),
      "codex",
      {
        sourceKind: "endpoint",
        endpointId: "ep-old",
      },
    );

    expect(clientMocks.selectProviderHarnessSource).toHaveBeenCalledWith("codex", "endpoint", "ep-old");
    expect(clientMocks.getProvidersBootstrap).toHaveBeenCalledWith(workspaceId);
    expect(store.getProvidersBootstrapSnapshot(workspaceId).provider_harness_config.codex?.selected_source_kind)
      .toBe("endpoint");
  });

  it("selectProviderSubscriptionAccount updates the active account and restores subscription source", async () => {
    const workspaceId = "ws-select-account";
    const nextAccounts = {
      active_account_id: "acct-b",
      accounts: [
        {
          id: "acct-a",
          label: "Account A",
          created_at: "2026-03-10T00:00:00Z",
        },
        {
          id: "acct-b",
          label: "Account B",
          created_at: "2026-03-10T00:00:00Z",
        },
      ],
      logins: [],
    };
    clientMocks.setCodexActiveAccount.mockResolvedValue(nextAccounts);
    clientMocks.selectProviderHarnessSource.mockImplementationOnce(async () => {
      workspaceSnapshots.set(workspaceId, makeWorkspaceBootstrap(workspaceId, {
        codex_accounts: nextAccounts,
        provider_harness_config: {
          codex: baseCodexConfig,
        },
      }));
      return baseCodexConfig;
    });

    const daemonConnection = await import("../api/daemonConnection");
    daemonConnection.setDaemonConnection({
      baseUrl: "https://daemon-select-account.example",
      source: "test",
    });
    const store = await import("./providersBootstrapStore");
    const actions = await import("./providerOnboardingActions");
    const scopeAdapters = await import("./providerScopeAdapters");

    store.updateProvidersBootstrap(workspaceId, () => makeWorkspaceBootstrap(workspaceId, {
      provider_harness_config: {
        codex: {
          ...baseCodexConfig,
          selected_source_kind: "endpoint",
          selected_endpoint_id: "ep-old",
        },
      },
    }));

    await actions.selectProviderSubscriptionAccount({
      ownerScope: scopeAdapters.getProviderOwnerScope(workspaceId),
      providerId: "codex",
      accountId: "acct-b",
      supportsEndpointConfig: true,
    });

    expect(clientMocks.setCodexActiveAccount).toHaveBeenCalledWith("acct-b");
    expect(clientMocks.selectProviderHarnessSource).toHaveBeenCalledWith("codex", "subscription", null);
    expect(store.getProvidersBootstrapSnapshot(workspaceId).codex_accounts.active_account_id).toBe("acct-b");
    expect(store.getProvidersBootstrapSnapshot(workspaceId).provider_harness_config.codex?.selected_source_kind)
      .toBe("subscription");
  });

  it("submitProviderEndpointAuth rolls back to the previous source when verification fails", async () => {
    const workspaceId = "ws-verify-failure";
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
    const selectedConfig: HarnessProviderSourceConfig = {
      ...afterUpsert,
      selected_source_kind: "endpoint",
      selected_endpoint_id: freshEndpoint.id,
    };

    clientMocks.upsertProviderHarnessEndpoint.mockResolvedValue(afterUpsert);
    clientMocks.selectProviderHarnessSource
      .mockImplementationOnce(async () => {
        workspaceSnapshots.set(workspaceId, makeWorkspaceBootstrap(workspaceId, {
          provider_harness_config: {
            codex: selectedConfig,
          },
        }));
        return selectedConfig;
      })
      .mockImplementationOnce(async () => {
        workspaceSnapshots.set(workspaceId, makeWorkspaceBootstrap(workspaceId));
        return baseCodexConfig;
      });
    clientMocks.verifyProviderForWorkspace.mockResolvedValue({
      provider_id: "codex",
      workspace_id: workspaceId,
      status: "failed",
      message: "bad endpoint",
    });

    const daemonConnection = await import("../api/daemonConnection");
    daemonConnection.setDaemonConnection({
      baseUrl: "https://daemon-rollback.example",
      source: "test",
    });
    const store = await import("./providersBootstrapStore");
    const actions = await import("./providerOnboardingActions");
    const scopeAdapters = await import("./providerScopeAdapters");

    store.updateProvidersBootstrap(workspaceId, () => makeWorkspaceBootstrap(workspaceId));

    const result = await actions.submitProviderEndpointAuth({
      ownerScope: scopeAdapters.getProviderOwnerScope(workspaceId),
      providerId: "codex",
      requestedEndpointId: null,
      name: freshEndpoint.name,
      baseUrl: freshEndpoint.base_url ?? null,
      apiShape: freshEndpoint.api_shape,
      authType: null,
      apiKey: "sk-test",
      serviceAccountJson: null,
      projectId: null,
      location: null,
      manualModelIds: [],
      previousSelection: {
        sourceKind: "subscription",
        endpointId: null,
      },
    });

    expect(result).toEqual({
      status: "rolled_back",
      selectedEndpointId: "ep-new",
      message: "bad endpoint",
    });
    expect(clientMocks.selectProviderHarnessSource).toHaveBeenNthCalledWith(1, "codex", "endpoint", "ep-new");
    expect(clientMocks.selectProviderHarnessSource).toHaveBeenNthCalledWith(2, "codex", "subscription", null);
    expect(store.getProvidersBootstrapSnapshot(workspaceId).provider_harness_config.codex?.selected_source_kind)
      .toBe("subscription");
    expect(analyticsMocks.trackProviderAuthStarted).toHaveBeenCalledWith({
      providerId: "codex",
      authMethod: "endpoint",
    });
    expect(analyticsMocks.trackProviderAuthFailed).toHaveBeenCalledWith({
      providerId: "codex",
      authMethod: "endpoint",
      failureKind: "verification_failed",
    });
    expect(analyticsMocks.trackProviderAuthCompleted).not.toHaveBeenCalled();
  });

  it("submitProviderEndpointAuth skips workspace verification for host scope and does not touch workspace bootstrap", async () => {
    const workspaceId = "ws-unchanged";
    const freshEndpoint = {
      ...baseEndpoint,
      id: "ep-host-new",
      name: "Host Secondary",
      updated_at: "2026-03-03T00:00:00Z",
    };
    const afterUpsert: HarnessProviderSourceConfig = {
      ...baseCodexConfig,
      endpoints: [...baseCodexConfig.endpoints, freshEndpoint],
    };
    const selectedConfig: HarnessProviderSourceConfig = {
      ...afterUpsert,
      selected_source_kind: "endpoint",
      selected_endpoint_id: freshEndpoint.id,
    };
    clientMocks.upsertProviderHarnessEndpoint.mockResolvedValue(afterUpsert);
    clientMocks.selectProviderHarnessSource.mockImplementationOnce(async () => {
      hostSnapshot = makeHostBootstrap({
        provider_harness_config: {
          codex: selectedConfig,
        },
      });
      return selectedConfig;
    });

    const daemonConnection = await import("../api/daemonConnection");
    daemonConnection.setDaemonConnection({
      baseUrl: "https://daemon-host.example",
      source: "test",
    });
    const store = await import("./providersBootstrapStore");
    const actions = await import("./providerOnboardingActions");
    const scopeAdapters = await import("./providerScopeAdapters");

    store.updateHostProvidersBootstrap(() => makeHostBootstrap());
    store.updateProvidersBootstrap(workspaceId, () => makeWorkspaceBootstrap(workspaceId));

    const result = await actions.submitProviderEndpointAuth({
      ownerScope: scopeAdapters.getProviderOwnerScope(null),
      providerId: "codex",
      requestedEndpointId: null,
      name: freshEndpoint.name,
      baseUrl: freshEndpoint.base_url ?? null,
      apiShape: freshEndpoint.api_shape,
      authType: null,
      apiKey: "sk-test",
      serviceAccountJson: null,
      projectId: null,
      location: null,
      manualModelIds: [],
      previousSelection: {
        sourceKind: "subscription",
        endpointId: null,
      },
    });

    expect(result).toEqual({
      status: "applied",
      selectedEndpointId: "ep-host-new",
    });
    expect(clientMocks.verifyProviderForWorkspace).not.toHaveBeenCalled();
    expect(store.getHostProvidersBootstrapSnapshot().provider_harness_config.codex?.selected_source_kind).toBe("endpoint");
    expect(store.getProvidersBootstrapSnapshot(workspaceId).provider_harness_config.codex?.selected_source_kind)
      .toBe("subscription");
    expect(analyticsMocks.trackProviderAuthStarted).toHaveBeenCalledWith({
      providerId: "codex",
      authMethod: "endpoint",
    });
    expect(analyticsMocks.trackProviderAuthCompleted).toHaveBeenCalledWith({
      providerId: "codex",
      authMethod: "endpoint",
    });
    expect(analyticsMocks.trackProviderAuthFailed).not.toHaveBeenCalled();
  });

  it("submitProviderEndpointAuth suppresses stale completion before selecting the endpoint source", async () => {
    const workspaceId = "ws-stale-before-select";
    const freshEndpoint = {
      ...baseEndpoint,
      id: "ep-stale",
      name: "Stale Endpoint",
      updated_at: "2026-03-04T00:00:00Z",
    };
    const afterUpsert: HarnessProviderSourceConfig = {
      ...baseCodexConfig,
      endpoints: [...baseCodexConfig.endpoints, freshEndpoint],
    };
    const staleFlag = { current: false };
    const upsertDeferred = deferred<HarnessProviderSourceConfig>();
    clientMocks.upsertProviderHarnessEndpoint.mockReturnValue(upsertDeferred.promise);

    const daemonConnection = await import("../api/daemonConnection");
    daemonConnection.setDaemonConnection({
      baseUrl: "https://daemon-stale-before-select.example",
      source: "test",
    });
    const store = await import("./providersBootstrapStore");
    const actions = await import("./providerOnboardingActions");
    const scopeAdapters = await import("./providerScopeAdapters");

    store.updateProvidersBootstrap(workspaceId, () => makeWorkspaceBootstrap(workspaceId));

    const resultPromise = actions.submitProviderEndpointAuth({
      ownerScope: scopeAdapters.getProviderOwnerScope(workspaceId),
      providerId: "codex",
      requestedEndpointId: null,
      name: freshEndpoint.name,
      baseUrl: freshEndpoint.base_url ?? null,
      apiShape: freshEndpoint.api_shape,
      authType: null,
      apiKey: "sk-test",
      serviceAccountJson: null,
      projectId: null,
      location: null,
      manualModelIds: [],
      previousSelection: {
        sourceKind: "subscription",
        endpointId: null,
      },
      isStale: () => staleFlag.current,
    });

    staleFlag.current = true;
    upsertDeferred.resolve(afterUpsert);

    await expect(resultPromise).resolves.toEqual({
      status: "stale",
      selectedEndpointId: "ep-stale",
    });
    expect(clientMocks.selectProviderHarnessSource).not.toHaveBeenCalled();
    expect(clientMocks.verifyProviderForWorkspace).not.toHaveBeenCalled();
    expect(analyticsMocks.trackProviderAuthFailed).toHaveBeenCalledWith({
      providerId: "codex",
      authMethod: "endpoint",
      failureKind: "user_cancelled",
    });
    expect(analyticsMocks.trackProviderAuthCompleted).not.toHaveBeenCalled();
  });

  it("submitProviderEndpointAuth suppresses stale rollback messaging when verification finishes stale", async () => {
    const workspaceId = "ws-stale-rollback";
    const freshEndpoint = {
      ...baseEndpoint,
      id: "ep-stale-rollback",
      name: "Rollback Endpoint",
      updated_at: "2026-03-05T00:00:00Z",
    };
    const afterUpsert: HarnessProviderSourceConfig = {
      ...baseCodexConfig,
      endpoints: [...baseCodexConfig.endpoints, freshEndpoint],
    };
    const selectedConfig: HarnessProviderSourceConfig = {
      ...afterUpsert,
      selected_source_kind: "endpoint",
      selected_endpoint_id: freshEndpoint.id,
    };
    const staleFlag = { current: false };
    const verifyDeferred = deferred<{
      provider_id: string;
      workspace_id: string;
      status: string;
      message?: string;
    }>();

    clientMocks.upsertProviderHarnessEndpoint.mockResolvedValue(afterUpsert);
    clientMocks.selectProviderHarnessSource
      .mockImplementationOnce(async () => {
        workspaceSnapshots.set(workspaceId, makeWorkspaceBootstrap(workspaceId, {
          provider_harness_config: {
            codex: selectedConfig,
          },
        }));
        return selectedConfig;
      })
      .mockImplementationOnce(async () => {
        workspaceSnapshots.set(workspaceId, makeWorkspaceBootstrap(workspaceId));
        return baseCodexConfig;
      });
    clientMocks.verifyProviderForWorkspace.mockReturnValue(verifyDeferred.promise);

    const daemonConnection = await import("../api/daemonConnection");
    daemonConnection.setDaemonConnection({
      baseUrl: "https://daemon-stale-rollback.example",
      source: "test",
    });
    const store = await import("./providersBootstrapStore");
    const actions = await import("./providerOnboardingActions");
    const scopeAdapters = await import("./providerScopeAdapters");

    store.updateProvidersBootstrap(workspaceId, () => makeWorkspaceBootstrap(workspaceId));

    const resultPromise = actions.submitProviderEndpointAuth({
      ownerScope: scopeAdapters.getProviderOwnerScope(workspaceId),
      providerId: "codex",
      requestedEndpointId: null,
      name: freshEndpoint.name,
      baseUrl: freshEndpoint.base_url ?? null,
      apiShape: freshEndpoint.api_shape,
      authType: null,
      apiKey: "sk-test",
      serviceAccountJson: null,
      projectId: null,
      location: null,
      manualModelIds: [],
      previousSelection: {
        sourceKind: "subscription",
        endpointId: null,
      },
      isStale: () => staleFlag.current,
    });

    await Promise.resolve();
    staleFlag.current = true;
    verifyDeferred.resolve({
      provider_id: "codex",
      workspace_id: workspaceId,
      status: "failed",
      message: "bad endpoint",
    });

    await expect(resultPromise).resolves.toEqual({
      status: "stale",
      selectedEndpointId: "ep-stale-rollback",
    });
    expect(clientMocks.selectProviderHarnessSource).toHaveBeenNthCalledWith(
      1,
      "codex",
      "endpoint",
      "ep-stale-rollback",
    );
    expect(clientMocks.selectProviderHarnessSource).toHaveBeenNthCalledWith(2, "codex", "subscription", null);
    expect(store.getProvidersBootstrapSnapshot(workspaceId).provider_harness_config.codex?.selected_source_kind)
      .toBe("subscription");
  });
});
