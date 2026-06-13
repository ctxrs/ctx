import { act, render, waitFor } from "@testing-library/react";
import { Fragment, createElement, useEffect } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ProviderOptions, ProvidersBootstrapResponse } from "../api/client";
import {
  getProviderHarnessConfig,
  getProviderOptions,
  getProvidersBootstrap,
  installAllProviders,
  installProvider,
  listAmpAccounts,
  listClaudeAccounts,
  listCodexAccounts,
  listCopilotAccounts,
  listCursorAccounts,
  listGeminiAccounts,
  listKimiAccounts,
  listMistralAccounts,
  listProviders,
  listQwenAccounts,
} from "../api/client";
import { setDaemonConnection } from "../api/daemonConnection";
import { observeInstall } from "./installProgressMonitor";
import {
  clearProviderInstallProgress,
  upsertProviderInstallProgressForScope,
} from "./providerInstallProgressStore";
import { getProviderOwnerScope } from "./providerScopeAdapters";
import { createDesktopLocalDaemonTargetScope } from "./scopeIdentity";
import {
  resetProviderOnboardingCoordinatorForTests,
  useProviderOnboardingCoordinator,
} from "./providerOnboardingCoordinator";
import {
  getProviderBootstrapTimeoutMessage,
  PROVIDER_BOOTSTRAP_TIMEOUT_MS,
} from "../utils/providerBootstrapTimeout";

const analyticsMocks = vi.hoisted(() => ({
  trackProviderInstallCompleted: vi.fn(),
  trackProviderInstallFailed: vi.fn(),
  trackProviderInstallStarted: vi.fn(),
}));

vi.mock("../api/client", async (importOriginal) => {
  const original = await importOriginal<typeof import("../api/client")>();
  return {
    ...original,
    getProviderHarnessConfig: vi.fn(),
    getProviderOptions: vi.fn(),
    getProvidersBootstrap: vi.fn(),
    installAllProviders: vi.fn(),
    installProvider: vi.fn(),
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
  };
});

vi.mock("../utils/analytics", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../utils/analytics")>();
  return {
    ...actual,
    ...analyticsMocks,
  };
});

vi.mock("./installProgressMonitor", async (importOriginal) => {
  const original = await importOriginal<typeof import("./installProgressMonitor")>();
  return {
    ...original,
    observeInstall: vi.fn(() => () => {}),
  };
});

type HookValue = ReturnType<typeof useProviderOnboardingCoordinator>;

const EMPTY_ACCOUNTS = Object.freeze({
  active_account_id: null,
  accounts: [],
});

const EMPTY_CODEX_ACCOUNTS = Object.freeze({
  active_account_id: null,
  accounts: [],
  logins: [],
});

const requireHookValue = (value: HookValue | null): HookValue => {
  if (!value) {
    throw new Error("hook value not ready");
  }
  return value;
};

const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
};

const baseOptions = (workspaceId: string, providerId: string): ProviderOptions => ({
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
});

const readyUsability = {
  usable: true,
  status: "ready",
  reason_code: null,
  reason: null,
  blocking_provider_ids: [],
  recommended_action: "none",
} as const;

const makeBootstrap = (
  workspaceId: string,
  overrides?: Partial<ProvidersBootstrapResponse>,
): ProvidersBootstrapResponse => ({
  providers: [
    {
      provider_id: "codex",
      display_name: "Codex",
      installed: false,
      health: "ok",
      diagnostics: [],
      details: {
        install_id: "install-codex",
        install_running: "true",
        install_target: "container",
      },
      usability: readyUsability,
    } as never,
  ],
  provider_options: {
    codex: baseOptions(workspaceId, "codex"),
  },
  provider_harness_config: {},
  codex_accounts: {
    active_account_id: "acct-a",
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
  amp_accounts: {
    active_account_id: null,
    accounts: [],
  },
  ...overrides,
});

const makeHostHarnessConfig = (providerId: string) => ({
  provider_id: providerId,
  selected_source_kind: "subscription" as const,
  selected_endpoint_id: null,
  endpoints: [],
});

const makeHostProvider = (overrides?: Record<string, unknown>) => ({
  provider_id: "codex",
  display_name: "Codex",
  installed: false,
  health: "ok",
  diagnostics: [],
  usability: readyUsability,
  details: {
    install_running: "false",
    install_target: "host",
  },
  ...overrides,
}) as never;

function CoordinatorHarness({
  workspaceId,
  onChange,
}: {
  workspaceId: string | null;
  onChange: (value: HookValue) => void;
}) {
  const value = useProviderOnboardingCoordinator({
    workspaceId,
    enabled: true,
  });

  useEffect(() => {
    onChange(value);
  }, [onChange, value]);

  return null;
}

beforeEach(() => {
  vi.clearAllMocks();
  resetProviderOnboardingCoordinatorForTests();
  clearProviderInstallProgress();
  analyticsMocks.trackProviderInstallCompleted.mockReset();
  analyticsMocks.trackProviderInstallFailed.mockReset();
  analyticsMocks.trackProviderInstallStarted.mockReset();
  vi.mocked(getProviderHarnessConfig).mockImplementation(async (providerId: string) =>
    makeHostHarnessConfig(providerId));
  vi.mocked(listProviders).mockResolvedValue([]);
  vi.mocked(installAllProviders).mockReset();
  vi.mocked(installProvider).mockReset();
  vi.mocked(listCodexAccounts).mockResolvedValue({ ...EMPTY_CODEX_ACCOUNTS });
  vi.mocked(listClaudeAccounts).mockResolvedValue({ ...EMPTY_ACCOUNTS });
  vi.mocked(listGeminiAccounts).mockResolvedValue({ ...EMPTY_ACCOUNTS });
  vi.mocked(listQwenAccounts).mockResolvedValue({ ...EMPTY_ACCOUNTS });
  vi.mocked(listKimiAccounts).mockResolvedValue({ ...EMPTY_ACCOUNTS });
  vi.mocked(listMistralAccounts).mockResolvedValue({ ...EMPTY_ACCOUNTS });
  vi.mocked(listCopilotAccounts).mockResolvedValue({ ...EMPTY_ACCOUNTS });
  vi.mocked(listCursorAccounts).mockResolvedValue({ ...EMPTY_ACCOUNTS });
  vi.mocked(listAmpAccounts).mockResolvedValue({ ...EMPTY_ACCOUNTS });
  setDaemonConnection({
    baseUrl: "https://daemon-a.example",
    source: "test",
  });
});

describe("providerOnboardingCoordinator", () => {
  it("shares running-install observation and foreground refresh across duplicate workspace subscribers", async () => {
    const workspaceId = "ws-shared-provider-onboarding";
    const stopInstallObservation = vi.fn();
    let firstValue: HookValue | null = null;
    let secondValue: HookValue | null = null;

    vi.mocked(observeInstall).mockReturnValue(stopInstallObservation);
    vi.mocked(getProvidersBootstrap).mockImplementation(async () => makeBootstrap(workspaceId));

    render(createElement(Fragment, {}, [
      createElement(CoordinatorHarness, {
        key: "first",
        workspaceId,
        onChange: (value) => {
          firstValue = value;
        },
      }),
      createElement(CoordinatorHarness, {
        key: "second",
        workspaceId,
        onChange: (value) => {
          secondValue = value;
        },
      }),
    ]));

    await waitFor(() => {
      expect(firstValue?.bootstrap.providers[0]?.details?.install_id).toBe("install-codex");
      expect(secondValue?.bootstrap.providers[0]?.details?.install_id).toBe("install-codex");
      expect(vi.mocked(observeInstall)).toHaveBeenCalledTimes(1);
      expect(vi.mocked(getProvidersBootstrap)).toHaveBeenCalledTimes(1);
    });

    vi.mocked(getProvidersBootstrap).mockClear();

    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });

    await waitFor(() => {
      expect(vi.mocked(getProvidersBootstrap)).toHaveBeenCalledTimes(1);
    });
  });

  it("refreshes host-scoped bootstrap on focus and online", async () => {
    let hookValue: HookValue | null = null;
    let currentProviders = [makeHostProvider()];

    vi.mocked(listProviders).mockImplementation(async () => currentProviders);

    render(createElement(CoordinatorHarness, {
      workspaceId: null,
      onChange: (value) => {
        hookValue = value;
      },
    }));

    await waitFor(() => {
      expect(hookValue?.bootstrap.providers[0]?.installed).toBe(false);
      expect(vi.mocked(listProviders)).toHaveBeenCalledTimes(1);
    });

    currentProviders = [makeHostProvider({ installed: true })];
    vi.mocked(listProviders).mockClear();

    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });

    await waitFor(() => {
      expect(hookValue?.bootstrap.providers[0]?.installed).toBe(true);
      expect(vi.mocked(listProviders)).toHaveBeenCalledTimes(1);
    });

    vi.mocked(listProviders).mockClear();

    await act(async () => {
      window.dispatchEvent(new Event("online"));
    });

    await waitFor(() => {
      expect(vi.mocked(listProviders)).toHaveBeenCalledTimes(1);
    });
  });

  it("refreshes bootstrap and hydrates provider options after a succeeded install", async () => {
    const workspaceId = "ws-post-install-followup";
    const stopInstallObservation = vi.fn();
    let currentBootstrap = makeBootstrap(workspaceId, {
      provider_options: {
        codex: {
          ...baseOptions(workspaceId, "codex"),
          models: {
            models: [{ id: "gpt-5.3-codex/low" }, { id: "gpt-5.3-codex/medium" }],
            current_model_id: "gpt-5.3-codex/medium",
            meta: {
              source_kind: "subscription",
              catalog_source: "codex_bundle_pinned",
              refresh_pending: true,
            },
          },
        },
      },
    });
    let hookValue: HookValue | null = null;

    vi.mocked(observeInstall).mockReturnValue(stopInstallObservation);
    vi.mocked(getProvidersBootstrap).mockImplementation(async () => currentBootstrap);
    vi.mocked(getProviderOptions).mockResolvedValue({
      ...baseOptions(workspaceId, "codex"),
      models: {
        models: [{ id: "gpt-5" }],
        current_model_id: "gpt-5",
      },
    });

    render(createElement(CoordinatorHarness, {
      workspaceId,
      onChange: (value) => {
        hookValue = value;
      },
    }));

    await waitFor(() => {
      expect(hookValue?.bootstrap.providers[0]?.details?.install_running).toBe("true");
      expect(vi.mocked(observeInstall)).toHaveBeenCalledTimes(1);
    });

    act(() => {
      upsertProviderInstallProgressForScope(getProviderOwnerScope(workspaceId), "codex", {
        installId: "install-codex",
        state: "running",
        pct: 50,
        target: "container",
      });
    });

    await waitFor(() => {
      expect(hookValue?.installsById.codex?.state).toBe("running");
    });

    currentBootstrap = makeBootstrap(workspaceId, {
      providers: [
        {
          provider_id: "codex",
          display_name: "Codex",
          installed: true,
          health: "ok",
          diagnostics: [],
          usability: readyUsability,
          details: {
            install_id: "install-codex",
            install_running: "false",
            install_target: "container",
          },
        } as never,
      ],
      provider_options: {
        codex: {
          ...baseOptions(workspaceId, "codex"),
          models: {
            models: [{ id: "gpt-5.3-codex/low" }, { id: "gpt-5.3-codex/medium" }],
            current_model_id: "gpt-5.3-codex/medium",
            meta: {
              source_kind: "subscription",
              catalog_source: "codex_bundle_pinned",
              refresh_pending: true,
            },
          },
        },
      },
    });

    act(() => {
      upsertProviderInstallProgressForScope(getProviderOwnerScope(workspaceId), "codex", {
        installId: "install-codex",
        state: "succeeded",
        pct: 100,
        target: "container",
      });
    });

    await waitFor(() => {
      expect(vi.mocked(getProviderOptions)).toHaveBeenCalledWith(workspaceId, "codex");
      expect(hookValue?.bootstrap.provider_options.codex?.models).toEqual({
        models: [{ id: "gpt-5" }],
        current_model_id: "gpt-5",
      });
      expect(stopInstallObservation).toHaveBeenCalled();
      expect(analyticsMocks.trackProviderInstallCompleted).toHaveBeenCalledWith({
        providerId: "codex",
        target: "container",
      });
    });
  });

  it("tracks provider install request start and API failure diagnostics", async () => {
    const workspaceId = "ws-install-analytics";
    let hookValue: HookValue | null = null;

    vi.mocked(getProvidersBootstrap).mockImplementation(async () => makeBootstrap(workspaceId));
    vi.mocked(installProvider).mockResolvedValueOnce({
      provider_id: "codex",
      install_id: "install-new",
      target: "container",
    });

    render(createElement(CoordinatorHarness, {
      workspaceId,
      onChange: (value) => {
        hookValue = value;
      },
    }));

    await waitFor(() => {
      expect(hookValue?.bootstrap.providers[0]?.provider_id).toBe("codex");
    });

    await act(async () => {
      await requireHookValue(hookValue).startProviderInstall("codex");
    });

    expect(vi.mocked(installProvider)).toHaveBeenCalledWith("codex", "container");
    expect(analyticsMocks.trackProviderInstallStarted).toHaveBeenCalledWith({
      providerId: "codex",
      target: "container",
    });

    vi.mocked(installProvider).mockRejectedValueOnce(new Error("install request failed"));

    await expect(requireHookValue(hookValue).startProviderInstall("codex")).rejects.toThrow(
      /install request failed/,
    );
    expect(analyticsMocks.trackProviderInstallFailed).toHaveBeenCalledWith({
      providerId: "codex",
      target: "container",
      failureKind: "request_failed",
    });
  });

  it("tracks bulk provider install request failure diagnostics", async () => {
    const workspaceId = "ws-bulk-install-analytics";
    let hookValue: HookValue | null = null;

    vi.mocked(getProvidersBootstrap).mockImplementation(async () => makeBootstrap(workspaceId));
    vi.mocked(installAllProviders).mockRejectedValueOnce(new Error("bulk install request failed"));

    render(createElement(CoordinatorHarness, {
      workspaceId,
      onChange: (value) => {
        hookValue = value;
      },
    }));

    await waitFor(() => {
      expect(hookValue?.bootstrap.providers[0]?.provider_id).toBe("codex");
    });

    await expect(requireHookValue(hookValue).startAllProviderInstalls()).rejects.toThrow(
      /bulk install request failed/,
    );

    expect(vi.mocked(installAllProviders)).toHaveBeenCalledWith("container");
    expect(analyticsMocks.trackProviderInstallFailed).toHaveBeenCalledWith({
      target: "container",
      failureKind: "request_failed",
    });
  });

  it("waits for a daemon target scope before loading workspace onboarding state", async () => {
    const workspaceId = "ws-target-scope-pending";
    let hookValue: HookValue | null = null;

    setDaemonConnection({
      baseUrl: "https://desktop-daemon.example",
      source: "desktop",
      targetScope: null,
    });
    vi.mocked(observeInstall).mockReturnValue(() => {});
    vi.mocked(getProvidersBootstrap).mockImplementation(async () => makeBootstrap(workspaceId));

    render(createElement(CoordinatorHarness, {
      workspaceId,
      onChange: (value) => {
        hookValue = value;
      },
    }));

    await waitFor(() => {
      expect(hookValue).not.toBeNull();
    });

    expect(requireHookValue(hookValue).bootstrap.providers).toEqual([]);
    expect(vi.mocked(getProvidersBootstrap)).not.toHaveBeenCalled();
    expect(vi.mocked(observeInstall)).not.toHaveBeenCalled();

    act(() => {
      setDaemonConnection({
        baseUrl: "https://desktop-daemon.example",
        source: "desktop",
        targetScope: createDesktopLocalDaemonTargetScope(),
      });
    });

    await waitFor(() => {
      expect(vi.mocked(getProvidersBootstrap)).toHaveBeenCalledTimes(1);
      expect(vi.mocked(observeInstall)).toHaveBeenCalledTimes(1);
      expect(hookValue?.bootstrap.provider_options.codex?.workspace_id).toBe(workspaceId);
    });
  });

  it("surfaces an explicit error when workspace bootstrap stalls past the timeout", async () => {
    const workspaceId = "ws-bootstrap-timeout";
    let hookValue: HookValue | null = null;

    vi.useFakeTimers();
    try {
      vi.mocked(getProvidersBootstrap).mockImplementation(
        () => new Promise(() => {}) as Promise<ProvidersBootstrapResponse>,
      );

      render(createElement(CoordinatorHarness, {
        workspaceId,
        onChange: (value) => {
          hookValue = value;
        },
      }));

      await act(async () => {
        await Promise.resolve();
      });

      expect(requireHookValue(hookValue).bootstrapState).toBe("loading");
      expect(requireHookValue(hookValue).bootstrapError).toBeNull();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(PROVIDER_BOOTSTRAP_TIMEOUT_MS);
        await Promise.resolve();
      });

      expect(requireHookValue(hookValue).bootstrapState).toBe("error");
      expect(requireHookValue(hookValue).bootstrapError).toBe(getProviderBootstrapTimeoutMessage());
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps a ready workspace stable while a background refresh is in flight", async () => {
    const workspaceId = "ws-refresh-stable";
    let hookValue: HookValue | null = null;
    const refreshDeferred = deferred<ProvidersBootstrapResponse>();

    vi.mocked(getProvidersBootstrap)
      .mockResolvedValueOnce(makeBootstrap(workspaceId))
      .mockImplementationOnce(() => refreshDeferred.promise);

    render(createElement(CoordinatorHarness, {
      workspaceId,
      onChange: (value) => {
        hookValue = value;
      },
    }));

    await waitFor(() => {
      expect(requireHookValue(hookValue).bootstrapState).toBe("ready");
      expect(requireHookValue(hookValue).bootstrapError).toBeNull();
    });

    let refreshPromise: Promise<ProvidersBootstrapResponse> | undefined;
    await act(async () => {
      refreshPromise = requireHookValue(hookValue).refreshBootstrap();
      await Promise.resolve();
    });

    expect(requireHookValue(hookValue).bootstrapState).toBe("ready");
    expect(requireHookValue(hookValue).bootstrapError).toBeNull();

    refreshDeferred.resolve(makeBootstrap(workspaceId, {
      provider_options: {
        codex: {
          ...baseOptions(workspaceId, "codex"),
          probed_at: "2026-03-10T00:00:01.000Z",
        },
      },
    }));

    await act(async () => {
      await refreshPromise;
    });

    await waitFor(() => {
      expect(requireHookValue(hookValue).bootstrapState).toBe("ready");
      expect(requireHookValue(hookValue).bootstrap.provider_options.codex?.probed_at).toBe(
        "2026-03-10T00:00:01.000Z",
      );
    });
  });

  it("preserves a ready workspace snapshot when a background refresh fails", async () => {
    const workspaceId = "ws-refresh-failure-stable";
    let hookValue: HookValue | null = null;
    const refreshDeferred = deferred<ProvidersBootstrapResponse>();

    vi.mocked(getProvidersBootstrap)
      .mockResolvedValueOnce(makeBootstrap(workspaceId))
      .mockImplementationOnce(() => refreshDeferred.promise);

    render(createElement(CoordinatorHarness, {
      workspaceId,
      onChange: (value) => {
        hookValue = value;
      },
    }));

    await waitFor(() => {
      expect(requireHookValue(hookValue).bootstrapState).toBe("ready");
    });

    let refreshPromise: Promise<ProvidersBootstrapResponse> | undefined;
    await act(async () => {
      refreshPromise = requireHookValue(hookValue).refreshBootstrap();
      await Promise.resolve();
    });

    expect(requireHookValue(hookValue).bootstrapState).toBe("ready");
    expect(requireHookValue(hookValue).bootstrapError).toBeNull();

    refreshDeferred.reject(new Error("background refresh failed"));

    await act(async () => {
      await refreshPromise?.catch(() => {});
    });

    expect(requireHookValue(hookValue).bootstrapState).toBe("ready");
    expect(requireHookValue(hookValue).bootstrapError).toBeNull();
    expect(requireHookValue(hookValue).bootstrap.provider_options.codex?.probed_at).toBe(
      "2026-03-10T00:00:00.000Z",
    );
  });

  it("does not share running installs or auth-summary dedupe across same-origin browser token changes", async () => {
    const workspaceId = "ws-daemon-target-scope";
    const pendingA = deferred<ProviderOptions>();
    const pendingB = deferred<ProviderOptions>();
    let hookValue: HookValue | null = null;
    const installedBootstrap = makeBootstrap(workspaceId, {
      providers: [
        {
          provider_id: "codex",
          display_name: "Codex",
          installed: true,
          health: "ok",
          diagnostics: [],
          usability: readyUsability,
          details: {
            install_id: "install-codex",
            install_running: "true",
            install_target: "container",
          },
        } as never,
      ],
    });

    setDaemonConnection({
      baseUrl: "https://daemon-a.example",
      authToken: "token-a",
      source: "same_origin_bootstrap",
    });
    vi.mocked(observeInstall).mockReturnValue(() => {});
    vi.mocked(getProvidersBootstrap).mockImplementation(async () => installedBootstrap);
    vi.mocked(getProviderOptions)
      .mockImplementationOnce(() => pendingA.promise)
      .mockImplementationOnce(() => pendingB.promise);

    render(createElement(CoordinatorHarness, {
      workspaceId,
      onChange: (value) => {
        hookValue = value;
      },
    }));

    await waitFor(() => {
      expect(vi.mocked(observeInstall)).toHaveBeenCalledTimes(1);
      expect(hookValue?.bootstrap.provider_options.codex?.workspace_id).toBe(workspaceId);
    });

    void requireHookValue(hookValue).ensureProviderAuthSummary("codex");
    await Promise.resolve();

    await waitFor(() => {
      expect(vi.mocked(getProviderOptions)).toHaveBeenCalledTimes(1);
    });

    act(() => {
      setDaemonConnection({
        baseUrl: "https://daemon-a.example",
        authToken: "token-b",
        source: "same_origin_bootstrap",
      });
    });

    await waitFor(() => {
      expect(vi.mocked(observeInstall)).toHaveBeenCalledTimes(2);
    });

    void requireHookValue(hookValue).ensureProviderAuthSummary("codex");
    await Promise.resolve();

    await waitFor(() => {
      expect(vi.mocked(getProviderOptions)).toHaveBeenCalledTimes(2);
    });

    pendingB.resolve({
      ...baseOptions(workspaceId, "codex"),
      models: {
        models: [{ id: "gpt-5" }],
        current_model_id: "gpt-5",
      },
    });
    pendingA.resolve(baseOptions(workspaceId, "codex"));

    await waitFor(() => {
      expect(hookValue?.bootstrap.provider_options.codex?.models).toEqual({
        models: [{ id: "gpt-5" }],
        current_model_id: "gpt-5",
      });
    });
  });

});
