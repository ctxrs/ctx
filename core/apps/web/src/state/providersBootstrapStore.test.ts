import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ProvidersBootstrapResponse } from "../api/client";

const clientMocks = vi.hoisted(() => ({
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
}));

vi.mock("../api/client", () => clientMocks);

const emptyAccounts = {
  active_account_id: null,
  accounts: [],
};

const makeWorkspaceBootstrap = (
  workspaceId: string,
  overrides?: Partial<ProvidersBootstrapResponse>,
): ProvidersBootstrapResponse => ({
  providers: [],
  provider_options: {
    codex: {
      provider_id: "codex",
      workspace_id: workspaceId,
      supports_load: false,
      auth_required: false,
      has_active_auth: true,
      auth_mode: "subscription" as const,
      account_identity: "acct-codex",
      source: {
        provider_id: "codex",
        selected_source_kind: "subscription" as const,
        selected_endpoint_id: null,
        endpoints: [],
      },
      probed_at: "2026-03-10T00:00:00.000Z",
    },
  },
  provider_harness_config: {},
  codex_accounts: {
    active_account_id: "acct-codex",
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

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
  clientMocks.listProviders.mockResolvedValue([
    {
      provider_id: "codex",
      display_name: "Codex",
      installed: true,
      health: "ok",
      diagnostics: [],
      details: {},
    },
    {
      provider_id: "claude-crp",
      display_name: "Claude",
      installed: true,
      health: "ok",
      diagnostics: [],
      details: {},
    },
  ]);
  clientMocks.getProviderHarnessConfig.mockImplementation(async (providerId: string) => ({
    provider_id: providerId,
    selected_source_kind: "subscription",
    selected_endpoint_id: null,
    endpoints: [],
  }));
  clientMocks.listCodexAccounts.mockResolvedValue({
    active_account_id: "acct-codex",
    accounts: [{ id: "acct-codex" }],
    logins: [],
  });
  clientMocks.listClaudeAccounts.mockResolvedValue({
    active_account_id: "acct-claude",
    accounts: [{ id: "acct-claude" }],
  });
  clientMocks.listGeminiAccounts.mockResolvedValue(emptyAccounts);
  clientMocks.listQwenAccounts.mockResolvedValue(emptyAccounts);
  clientMocks.listKimiAccounts.mockResolvedValue(emptyAccounts);
  clientMocks.listMistralAccounts.mockResolvedValue(emptyAccounts);
  clientMocks.listCopilotAccounts.mockResolvedValue(emptyAccounts);
  clientMocks.listCursorAccounts.mockResolvedValue(emptyAccounts);
  clientMocks.listAmpAccounts.mockResolvedValue(emptyAccounts);
  clientMocks.getProvidersBootstrap.mockResolvedValue(undefined);
});

describe("providersBootstrapStore", () => {
  it("loads host bootstrap with host auth and harness-config slices", async () => {
    const store = await import("./providersBootstrapStore");

    const bootstrap = await store.loadHostProvidersBootstrap();

    expect(clientMocks.listProviders).toHaveBeenCalledWith("host");
    expect(clientMocks.getProviderHarnessConfig).toHaveBeenCalledTimes(2);
    expect(bootstrap.codex_accounts.active_account_id).toBe("acct-codex");
    expect(bootstrap.claude_accounts.active_account_id).toBe("acct-claude");
    expect(bootstrap.provider_harness_config.codex).toMatchObject({
      provider_id: "codex",
      selected_source_kind: "subscription",
    });
    expect(store.getHostProvidersBootstrapSnapshot().provider_harness_config["claude-crp"]).toMatchObject({
      provider_id: "claude-crp",
    });
  });

  it("refreshes host bootstrap slices instead of leaving stale host auth in place", async () => {
    const store = await import("./providersBootstrapStore");

    await store.loadHostProvidersBootstrap();
    clientMocks.listCodexAccounts.mockResolvedValueOnce({
      active_account_id: "acct-codex-next",
      accounts: [{ id: "acct-codex-next" }],
      logins: [],
    });

    const refreshed = await store.refreshHostProvidersBootstrap();

    expect(refreshed.codex_accounts.active_account_id).toBe("acct-codex-next");
    expect(store.getHostProvidersBootstrapSnapshot().codex_accounts.active_account_id).toBe("acct-codex-next");
  });

  it("keeps same workspace ids isolated across daemon target scopes", async () => {
    const store = await import("./providersBootstrapStore");
    const daemonConnection = await import("../api/daemonConnection");

    daemonConnection.setDaemonConnection({
      baseUrl: "https://daemon-a.example",
      source: "test",
    });
    clientMocks.getProvidersBootstrap.mockResolvedValueOnce(makeWorkspaceBootstrap("ws-same-scope", {
      provider_options: {
        codex: {
          ...makeWorkspaceBootstrap("ws-same-scope").provider_options.codex,
          probed_at: "2026-03-10T00:00:01.000Z",
          models: {
            models: [{ id: "gpt-5" }],
            current_model_id: "gpt-5",
          },
        },
      },
    }));
    await store.loadProvidersBootstrap("ws-same-scope");

    daemonConnection.setDaemonConnection({
      baseUrl: "https://daemon-b.example",
      source: "test",
    });
    clientMocks.getProvidersBootstrap.mockResolvedValueOnce(makeWorkspaceBootstrap("ws-same-scope", {
      provider_options: {
        codex: {
          ...makeWorkspaceBootstrap("ws-same-scope").provider_options.codex,
          probed_at: "2026-03-10T00:00:02.000Z",
          account_identity: "acct-daemon-b",
        },
      },
      codex_accounts: {
        active_account_id: "acct-daemon-b",
        accounts: [],
        logins: [],
      },
    }));
    await store.loadProvidersBootstrap("ws-same-scope");

    expect(store.getProvidersBootstrapSnapshot("ws-same-scope").provider_options.codex?.account_identity).toBe("acct-daemon-b");
    expect(store.getProvidersBootstrapSnapshot("ws-same-scope").provider_options.codex?.probed_at).toBe("2026-03-10T00:00:02.000Z");

    daemonConnection.setDaemonConnection({
      baseUrl: "https://daemon-a.example",
      source: "test",
    });

    expect(store.getProvidersBootstrapSnapshot("ws-same-scope").provider_options.codex?.models).toEqual({
      models: [{ id: "gpt-5" }],
      current_model_id: "gpt-5",
    });
    expect(store.getProvidersBootstrapSnapshot("ws-same-scope").provider_options.codex?.account_identity).toBe("acct-codex");
  });

  it("keeps host and workspace bootstrap caches distinct on the same daemon target", async () => {
    const store = await import("./providersBootstrapStore");
    const daemonConnection = await import("../api/daemonConnection");

    daemonConnection.setDaemonConnection({
      baseUrl: "https://daemon-host.example",
      source: "test",
    });

    store.updateHostProvidersBootstrap((current) => ({
      ...current,
      codex_accounts: {
        active_account_id: "acct-host",
        accounts: [],
        logins: [],
      },
    }));
    store.updateProvidersBootstrap("ws-owner-scope", (current) => ({
      ...current,
      codex_accounts: {
        active_account_id: "acct-workspace",
        accounts: [],
        logins: [],
      },
      provider_options: makeWorkspaceBootstrap("ws-owner-scope").provider_options,
    }));

    expect(store.getHostProvidersBootstrapSnapshot().codex_accounts.active_account_id).toBe("acct-host");
    expect(store.getProvidersBootstrapSnapshot("ws-owner-scope").codex_accounts.active_account_id).toBe("acct-workspace");
    expect(store.getHostProvidersBootstrapSnapshot().provider_options.codex).toBeUndefined();
    expect(store.getProvidersBootstrapSnapshot("ws-owner-scope").provider_options.codex?.workspace_id).toBe("ws-owner-scope");
  });

  it("preserves a live model catalog when bootstrap only has a pinned placeholder catalog", async () => {
    const store = await import("./providersBootstrapStore");

    const previous = {
      provider_id: "codex",
      workspace_id: "ws-test",
      supports_load: false,
      auth_required: false,
      has_active_auth: true,
      auth_mode: "subscription" as const,
      account_identity: "acct-codex",
      source: {
        provider_id: "codex",
        selected_source_kind: "subscription" as const,
        selected_endpoint_id: null,
        endpoints: [],
      },
      probed_at: "2026-03-10T00:00:00.000Z",
      probe_ok: true,
      models: {
        models: [{ id: "gpt-5.4/low" }, { id: "gpt-5.4/medium" }],
        current_model_id: "gpt-5.4/medium",
      },
    };
    const next = {
      ...previous,
      probed_at: "2026-03-10T00:05:00.000Z",
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

    const resolved = store.resolveProviderOptionsUpdate(previous, next);

    expect(resolved?.models).toEqual(previous.models);
  });

  it("clears preferred_model_id when a partial refresh omits the server field", async () => {
    const store = await import("./providersBootstrapStore");
    const daemonConnection = await import("../api/daemonConnection");

    daemonConnection.setDaemonConnection({
      baseUrl: "https://daemon-pref.example",
      source: "test",
    });
    clientMocks.getProvidersBootstrap.mockResolvedValueOnce(makeWorkspaceBootstrap("ws-pref", {
      provider_options: {
        codex: {
          ...makeWorkspaceBootstrap("ws-pref").provider_options.codex,
          preferred_model_id: "gpt-5.4/xhigh",
          probed_at: "2026-03-10T00:00:01.000Z",
          models: {
            models: [{ id: "gpt-5.4/medium" }, { id: "gpt-5.4/xhigh" }],
            current_model_id: "gpt-5.4/medium",
          },
        },
      },
    }));
    await store.loadProvidersBootstrap("ws-pref");

    clientMocks.getProvidersBootstrap.mockResolvedValueOnce(makeWorkspaceBootstrap("ws-pref", {
      provider_options: {
        codex: {
          ...makeWorkspaceBootstrap("ws-pref").provider_options.codex,
          probed_at: "2026-03-10T00:00:01.000Z",
        },
      },
    }));
    await store.refreshProvidersBootstrap("ws-pref");

    expect(store.getProvidersBootstrapSnapshot("ws-pref").provider_options.codex?.models).toEqual({
      models: [{ id: "gpt-5.4/medium" }, { id: "gpt-5.4/xhigh" }],
      current_model_id: "gpt-5.4/medium",
    });
    expect(store.getProvidersBootstrapSnapshot("ws-pref").provider_options.codex?.preferred_model_id).toBeUndefined();
  });

  it("does not preserve preferred_model_id across a fresh probe without a server field", async () => {
    const store = await import("./providersBootstrapStore");
    const daemonConnection = await import("../api/daemonConnection");

    daemonConnection.setDaemonConnection({
      baseUrl: "https://daemon-pref-clear.example",
      source: "test",
    });
    clientMocks.getProvidersBootstrap.mockResolvedValueOnce(makeWorkspaceBootstrap("ws-pref-clear", {
      provider_options: {
        codex: {
          ...makeWorkspaceBootstrap("ws-pref-clear").provider_options.codex,
          preferred_model_id: "gpt-5.4/xhigh",
          probed_at: "2026-03-10T00:00:01.000Z",
          models: {
            models: [{ id: "gpt-5.4/medium" }, { id: "gpt-5.4/xhigh" }],
            current_model_id: "gpt-5.4/medium",
          },
        },
      },
    }));
    await store.loadProvidersBootstrap("ws-pref-clear");

    clientMocks.getProvidersBootstrap.mockResolvedValueOnce(makeWorkspaceBootstrap("ws-pref-clear", {
      provider_options: {
        codex: {
          ...makeWorkspaceBootstrap("ws-pref-clear").provider_options.codex,
          probed_at: "2026-03-10T00:00:02.000Z",
        },
      },
    }));
    await store.refreshProvidersBootstrap("ws-pref-clear");

    expect(store.getProvidersBootstrapSnapshot("ws-pref-clear").provider_options.codex?.preferred_model_id).toBeUndefined();
  });
});
