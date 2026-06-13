import { act, render, waitFor } from "@testing-library/react";
import { createElement, useEffect, useState, type Dispatch, type SetStateAction } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { DraftHarness } from "../../components/WorkbenchComposer";
import type { ProviderOptions, ProviderStatus, ProvidersBootstrapResponse } from "../../api/client";
import { getHealth, getProviderOptions, getProvidersBootstrap, installAllProviders, installProvider } from "../../api/client";
import { setDaemonConnection } from "../../api/daemonConnection";
import {
  getProviderInstallProgressSnapshotForScope,
} from "../../state/providerInstallProgressStore";
import { resetProviderOnboardingCoordinatorForTests } from "../../state/providerOnboardingCoordinator";
import { invalidateProvidersBootstrap, refreshProvidersBootstrap } from "../../state/providersBootstrapStore";
import { getProviderOwnerScopeKeyOrNull } from "../../state/providerScopeAdapters";
import { readAcknowledgedProviderRuntimeWarningIds } from "../../utils/providerRuntimeWarnings";
import { resolveProviderOptionsUpdate, shouldHydrateProviderModels } from "./useWorkbenchProviders";
import { useWorkbenchProviders } from "./useWorkbenchProviders";

vi.mock("../../api/client", async (importOriginal) => {
  const original = await importOriginal<typeof import("../../api/client")>();
  return {
    ...original,
    getHealth: vi.fn(),
    getProviderOptions: vi.fn(),
    getProvidersBootstrap: vi.fn(),
    installProvider: vi.fn(),
    installAllProviders: vi.fn(),
  };
});

vi.mock("../../state/providerInstallProgressStore", async (importOriginal) => {
  const original = await importOriginal<typeof import("../../state/providerInstallProgressStore")>();
  return {
    ...original,
    getProviderInstallProgressSnapshot: vi.fn(() => ({})),
    getProviderInstallProgressSnapshotForScope: vi.fn(() => ({})),
    subscribeProviderInstallProgress: vi.fn(() => () => {}),
    subscribeProviderInstallProgressForScope: vi.fn(() => () => {}),
  };
});

vi.mock("../../state/installProgressMonitor", async (importOriginal) => {
  const original = await importOriginal<typeof import("../../state/installProgressMonitor")>();
  return {
    ...original,
    observeInstall: vi.fn(() => () => {}),
  };
});

const baseOptions = (providerId: string): ProviderOptions => ({
  provider_id: providerId,
  workspace_id: "ws-test",
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
  probed_at: new Date().toISOString(),
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

const providerStatus = (
  provider_id: string,
  opts?: Partial<ProviderStatus>,
): ProviderStatus => ({
  provider_id,
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
  ...opts,
});

const makeBootstrap = (
  providerOptions: Record<string, ProviderOptions>,
  providers: ProviderStatus[] = [providerStatus("codex")],
): ProvidersBootstrapResponse => ({
  providers,
  provider_options: providerOptions,
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
});

type HookValue = ReturnType<typeof useWorkbenchProviders>;

const noopSetDraftHarness: Dispatch<SetStateAction<DraftHarness | null>> = () => undefined;

function WorkbenchProvidersHarness({
  workspaceId,
  onChange,
}: {
  workspaceId: string;
  onChange: (value: HookValue) => void;
}) {
  const value = useWorkbenchProviders({
    workspaceId,
    setDraftHarness: noopSetDraftHarness,
    onStartError: () => {},
  });

  useEffect(() => {
    onChange(value);
  }, [onChange, value]);

  return null;
}

function WorkbenchProvidersDraftHarness({
  workspaceId,
  initialDraftHarness,
  onChange,
}: {
  workspaceId: string;
  initialDraftHarness: DraftHarness | null;
  onChange: (value: { hookValue: HookValue; draftHarness: DraftHarness | null }) => void;
}) {
  const [draftHarness, setDraftHarness] = useState<DraftHarness | null>(initialDraftHarness);
  const hookValue = useWorkbenchProviders({
    workspaceId,
    setDraftHarness,
    onStartError: () => {},
  });

  useEffect(() => {
    onChange({ hookValue, draftHarness });
  }, [draftHarness, hookValue, onChange]);

  return null;
}

beforeEach(() => {
  vi.clearAllMocks();
  resetProviderOnboardingCoordinatorForTests();
  window.localStorage.clear();
  window.sessionStorage.clear();
  setDaemonConnection({
    baseUrl: "https://daemon-a.example",
    source: "test",
  });
  vi.mocked(getHealth).mockResolvedValue({
    version: "0.45.0",
    daemon_version: "0.45.0",
    pid: 1,
    data_root: "/tmp/ctx",
    daemon_url: "https://daemon-a.example",
    auth_required: false,
    compatibility: {
      desktop_exact_version: "0.45.0",
      desktop_build_id: "build-a",
      desktop_dev_instance_id: "dev-a",
      mobile_api_min: 1,
      mobile_api_max: 1,
    },
  });
});

describe("useWorkbenchProviders bootstrap state", () => {
  it("reports ready after the initial provider bootstrap resolves", async () => {
    vi.mocked(getProvidersBootstrap).mockResolvedValue(makeBootstrap({ codex: baseOptions("codex") }) as never);

    let hookValue: HookValue | null = null;
    render(
      createElement(WorkbenchProvidersHarness, {
        workspaceId: "ws-test",
        onChange: (value) => {
          hookValue = value;
        },
      }),
    );

    await waitFor(() => {
      expect(hookValue?.bootstrapState).toBe("ready");
      expect(hookValue?.bootstrapError).toBeNull();
    });
  });

  it("refreshes provider bootstrap once after an app build change", async () => {
    vi.mocked(getProvidersBootstrap).mockResolvedValue(makeBootstrap({ codex: baseOptions("codex") }) as never);
    invalidateProvidersBootstrap("ws-test");
    const firstRender = render(
      createElement(WorkbenchProvidersHarness, {
        workspaceId: "ws-test",
        onChange: () => {},
      }),
    );

    await waitFor(() => {
      expect(vi.mocked(getProvidersBootstrap)).toHaveBeenCalledTimes(1);
    });

    firstRender.unmount();
    window.localStorage.setItem("ctx.provider_runtime.checked_build.ws-test", "build-old");

    render(
      createElement(WorkbenchProvidersHarness, {
        workspaceId: "ws-test",
        onChange: () => {},
      }),
    );

    await waitFor(() => {
      expect(vi.mocked(getProvidersBootstrap)).toHaveBeenCalledTimes(2);
    });
    expect(window.localStorage.getItem("ctx.provider_runtime.checked_build.ws-test")).toBe("build-a");
  });
});

describe("shouldHydrateProviderModels", () => {
  it("requests hydration for claude subscription auth when models are missing", () => {
    expect(shouldHydrateProviderModels("claude-crp", baseOptions("claude-crp"))).toBe(true);
  });

  it("does not request subscription hydration for endpoint-selected sources when models are missing", () => {
    const options: ProviderOptions = {
      ...baseOptions("claude-crp"),
      source: {
        provider_id: "claude-crp",
        selected_source_kind: "endpoint",
        selected_endpoint_id: "ep-1",
        endpoints: [],
      },
    };
    expect(shouldHydrateProviderModels("claude-crp", options)).toBe(false);
  });

  it("does not request hydration when models already exist", () => {
    const options: ProviderOptions = {
      ...baseOptions("claude-crp"),
      models: {
        models: [{ id: "anthropic/claude-sonnet-4.5" }],
        current_model_id: "anthropic/claude-sonnet-4.5",
        meta: {
          source_kind: "subscription",
          catalog_source: "runtime_probe_live",
          refresh_pending: false,
        },
      },
    };
    expect(shouldHydrateProviderModels("claude-crp", options)).toBe(false);
  });

  it("keeps hydrating when discovery models exist but are still provisional", () => {
    const options: ProviderOptions = {
      ...baseOptions("codex"),
      models: {
        models: [{ id: "gpt-5.4/medium" }],
        current_model_id: "gpt-5.4/medium",
        meta: {
          source_kind: "subscription",
          catalog_source: "codex_bundle_pinned",
          refresh_pending: true,
        },
      },
    };
    expect(shouldHydrateProviderModels("codex", options)).toBe(true);
  });

  it("requests hydration for gemini subscription auth when models are missing", () => {
    expect(shouldHydrateProviderModels("gemini", baseOptions("gemini"))).toBe(true);
  });

  it("requests hydration for copilot subscription auth when models are missing", () => {
    expect(shouldHydrateProviderModels("copilot", baseOptions("copilot"))).toBe(true);
  });

  it("requests hydration for cursor subscription auth when models are missing", () => {
    expect(shouldHydrateProviderModels("cursor", baseOptions("cursor"))).toBe(true);
  });

  it("requests hydration for qwen subscription auth when models are missing", () => {
    for (const providerId of ["qwen"]) {
      expect(shouldHydrateProviderModels(providerId, baseOptions(providerId))).toBe(true);
    }
  });

  it("requests hydration for kimi subscription auth when models are missing", () => {
    expect(shouldHydrateProviderModels("kimi", baseOptions("kimi"))).toBe(true);
  });

  it("requests hydration for amp subscription auth when models are missing", () => {
    expect(shouldHydrateProviderModels("amp", baseOptions("amp"))).toBe(true);
  });

  it("does not request hydration for providers outside the live subscription discovery set", () => {
    expect(shouldHydrateProviderModels("mistral", baseOptions("mistral"))).toBe(false);
    expect(shouldHydrateProviderModels("auggie", baseOptions("auggie"))).toBe(false);
  });

  it("does not keep passively hydrating after a failed probe", () => {
    const options: ProviderOptions = {
      ...baseOptions("codex"),
      probe_ok: false,
      probe_error: "crp runtime closed before models.list response",
    };
    expect(shouldHydrateProviderModels("codex", options)).toBe(false);
  });

  it("allows an explicit retry after a failed probe", () => {
    const options: ProviderOptions = {
      ...baseOptions("codex"),
      probe_ok: false,
      probe_error: "crp runtime closed before models.list response",
    };
    expect(shouldHydrateProviderModels("codex", options, "explicit")).toBe(true);
  });
});

describe("resolveProviderOptionsUpdate", () => {
  it("preserves last known models when a later payload drops them for the same source", () => {
    const previous: ProviderOptions = {
      ...baseOptions("codex"),
      models: {
        models: [{ id: "gpt-5" }],
        current_model_id: "gpt-5",
      },
    };
    const next: ProviderOptions = {
      ...baseOptions("codex"),
      probed_at: "2026-03-09T00:00:05.000Z",
      probe_ok: false,
      probe_error: "crp runtime closed before models.list response",
    };

    expect(resolveProviderOptionsUpdate(previous, next)).toEqual({
      ...next,
      models: previous.models,
    });
  });

  it("keeps a failed probe sticky across bootstrap summaries until a real retry", () => {
    const previous: ProviderOptions = {
      ...baseOptions("codex"),
      probe_ok: false,
      probe_error: "crp runtime closed before models.list response",
    };
    const next: ProviderOptions = {
      ...baseOptions("codex"),
      probed_at: "2026-03-09T00:00:05.000Z",
    };

    expect(resolveProviderOptionsUpdate(previous, next)).toEqual({
      ...next,
      probe_ok: false,
      probe_error: previous.probe_error,
    });
  });

  it("drops preserved models and probe state when subscription account identity changes", () => {
    const previous: ProviderOptions = {
      ...baseOptions("codex"),
      account_identity: "acct-a",
      models: {
        models: [{ id: "gpt-5" }],
        current_model_id: "gpt-5",
      },
      probe_ok: false,
      probe_error: "stale probe failure",
    };
    const next: ProviderOptions = {
      ...baseOptions("codex"),
      account_identity: "acct-b",
      probed_at: "2026-03-09T00:00:05.000Z",
    };

    expect(resolveProviderOptionsUpdate(previous, next)).toEqual(next);
  });

  it("drops preserved models when endpoint credentials rotate on the same endpoint", () => {
    const previous: ProviderOptions = {
      ...baseOptions("claude-crp"),
      auth_mode: "endpoint",
      source: {
        provider_id: "claude-crp",
        selected_source_kind: "endpoint",
        selected_endpoint_id: "ep-1",
        endpoints: [
          {
            id: "ep-1",
            provider_id: "claude-crp",
            name: "Primary",
            base_url: "https://api.example.test",
            api_shape: "anthropic_messages",
            auth_type: "bearer",
            model_override: null,
            created_at: "2026-03-09T00:00:00.000Z",
            updated_at: "2026-03-09T00:00:00.000Z",
            last_verification_status: "valid",
            last_verification_at: null,
            last_error: null,
            has_api_key: true,
          },
        ],
      },
      models: {
        models: [{ id: "claude-sonnet-4.5" }],
        current_model_id: "claude-sonnet-4.5",
      },
    };
    const next: ProviderOptions = {
      ...previous,
      probed_at: "2026-03-09T00:00:05.000Z",
      models: undefined,
      source: {
        ...previous.source!,
        endpoints: [
          {
            ...previous.source!.endpoints[0]!,
            updated_at: "2026-03-09T00:01:00.000Z",
          },
        ],
      },
    };

    expect(resolveProviderOptionsUpdate(previous, next)).toEqual(next);
  });
});

describe("useWorkbenchProviders", () => {
  it("hydrates provider details into the shared scoped resource without losing account identity", async () => {
    const workspaceId = "ws-auth-summary";
    let currentBootstrap = makeBootstrap({
      codex: {
        ...baseOptions("codex"),
        workspace_id: workspaceId,
      },
    });
    let hookValue: HookValue | null = null;

    vi.mocked(getProvidersBootstrap).mockImplementation(async () => currentBootstrap);
    vi.mocked(getProviderOptions).mockResolvedValue({
      ...baseOptions("codex"),
      workspace_id: workspaceId,
      models: {
        models: [{ id: "gpt-5" }],
        current_model_id: "gpt-5",
      },
    });

    render(createElement(WorkbenchProvidersHarness, {
      workspaceId,
      onChange: (next) => {
        hookValue = next;
      },
    }));

    await waitFor(() => {
      expect(hookValue?.providerOptions.codex?.account_identity).toBe("acct-a");
    });

    await act(async () => {
      await hookValue?.ensureProviderAuthSummary("codex");
    });

    await waitFor(() => {
      expect(hookValue?.providerOptions.codex?.models).toEqual({
        models: [{ id: "gpt-5" }],
        current_model_id: "gpt-5",
      });
      expect(hookValue?.providerOptions.codex?.account_identity).toBe("acct-a");
    });

    currentBootstrap = makeBootstrap({
      codex: {
        ...baseOptions("codex"),
        workspace_id: workspaceId,
        probed_at: "2026-03-10T00:00:05.000Z",
      },
    });

    await act(async () => {
      await refreshProvidersBootstrap(workspaceId);
    });

    await waitFor(() => {
      expect(hookValue?.providerOptions.codex?.models).toEqual({
        models: [{ id: "gpt-5" }],
        current_model_id: "gpt-5",
      });
      expect(hookValue?.providerOptions.codex?.account_identity).toBe("acct-a");
    });
  });

  it("drops preserved models when the workspace-scoped auth identity changes", async () => {
    const workspaceId = "ws-auth-identity-change";
    let currentBootstrap = makeBootstrap({
      codex: {
        ...baseOptions("codex"),
        workspace_id: workspaceId,
      },
    });
    let hookValue: HookValue | null = null;

    vi.mocked(getProvidersBootstrap).mockImplementation(async () => currentBootstrap);
    vi.mocked(getProviderOptions).mockResolvedValue({
      ...baseOptions("codex"),
      workspace_id: workspaceId,
      models: {
        models: [{ id: "gpt-5" }],
        current_model_id: "gpt-5",
      },
    });

    render(createElement(WorkbenchProvidersHarness, {
      workspaceId,
      onChange: (next) => {
        hookValue = next;
      },
    }));

    await waitFor(() => {
      expect(hookValue?.providerOptions.codex?.account_identity).toBe("acct-a");
    });

    await act(async () => {
      await hookValue?.ensureProviderAuthSummary("codex");
    });

    await waitFor(() => {
      expect(hookValue?.providerOptions.codex?.models).toBeDefined();
    });

    currentBootstrap = {
      ...makeBootstrap({
        codex: {
          ...baseOptions("codex"),
          workspace_id: workspaceId,
          probed_at: "2026-03-10T00:00:05.000Z",
        },
      }),
      codex_accounts: {
        active_account_id: "acct-b",
        accounts: [],
        logins: [],
      },
    };

    await act(async () => {
      await refreshProvidersBootstrap(workspaceId);
    });

    await waitFor(() => {
      expect(hookValue?.providerOptions.codex?.account_identity).toBe("acct-b");
    });

    expect(requireHookValue(hookValue).providerOptions.codex?.models).toBeUndefined();
  });

  it("scopes in-flight provider auth-summary requests by workspace", async () => {
    const pendingByWorkspace = new Map([
      ["ws-a", deferred<ProviderOptions>()],
      ["ws-b", deferred<ProviderOptions>()],
    ]);
    let hookValue: HookValue | null = null;

    vi.mocked(getProvidersBootstrap).mockImplementation(async (workspaceId: string) => makeBootstrap({
      codex: {
        ...baseOptions("codex"),
        workspace_id: workspaceId,
      },
    }));
    vi.mocked(getProviderOptions).mockImplementation((workspaceId: string, _providerId: string) => {
      const pending = pendingByWorkspace.get(workspaceId);
      if (!pending) {
        throw new Error(`missing pending provider options for ${workspaceId}`);
      }
      return pending.promise;
    });

    const renderResult = render(createElement(WorkbenchProvidersHarness, {
      workspaceId: "ws-a",
      onChange: (next) => {
        hookValue = next;
      },
    }));

    await waitFor(() => {
      expect(hookValue?.providerOptions.codex?.workspace_id).toBe("ws-a");
    });

    void requireHookValue(hookValue).ensureProviderAuthSummary("codex");
    await Promise.resolve();

    await waitFor(() => {
      expect(vi.mocked(getProviderOptions)).toHaveBeenCalledWith("ws-a", "codex");
    });

    renderResult.rerender(createElement(WorkbenchProvidersHarness, {
      workspaceId: "ws-b",
      onChange: (next) => {
        hookValue = next;
      },
    }));

    await waitFor(() => {
      expect(hookValue?.providerOptions.codex?.workspace_id).toBe("ws-b");
    });

    void requireHookValue(hookValue).ensureProviderAuthSummary("codex");
    await Promise.resolve();

    await waitFor(() => {
      expect(vi.mocked(getProviderOptions).mock.calls.some(
        ([workspaceId, providerId]) => workspaceId === "ws-b" && providerId === "codex",
      )).toBe(true);
    });

    pendingByWorkspace.get("ws-b")?.resolve({
      ...baseOptions("codex"),
      workspace_id: "ws-b",
      models: {
        models: [{ id: "gpt-5" }],
        current_model_id: "gpt-5",
      },
    });
    pendingByWorkspace.get("ws-a")?.resolve({
      ...baseOptions("codex"),
      workspace_id: "ws-a",
    });

    await waitFor(() => {
      expect(hookValue?.providerOptions.codex?.workspace_id).toBe("ws-b");
      expect(hookValue?.providerOptions.codex?.models).toEqual({
        models: [{ id: "gpt-5" }],
        current_model_id: "gpt-5",
      });
    });
  });

  it("scopes in-flight provider auth-summary requests by daemon target for the same workspace id", async () => {
    const workspaceId = "ws-daemon-scope";
    const pendingA = deferred<ProviderOptions>();
    const pendingB = deferred<ProviderOptions>();
    let hookValue: HookValue | null = null;

    vi.mocked(getProvidersBootstrap).mockImplementation(async () => makeBootstrap({
      codex: {
        ...baseOptions("codex"),
        workspace_id: workspaceId,
      },
    }));
    vi.mocked(getProviderOptions)
      .mockImplementationOnce(() => pendingA.promise)
      .mockImplementationOnce(() => pendingB.promise);

    render(createElement(WorkbenchProvidersHarness, {
      workspaceId,
      onChange: (next) => {
        hookValue = next;
      },
    }));

    await waitFor(() => {
      expect(hookValue?.providerOptions.codex?.workspace_id).toBe(workspaceId);
    });

    void requireHookValue(hookValue).ensureProviderAuthSummary("codex");
    await Promise.resolve();

    await waitFor(() => {
      expect(vi.mocked(getProviderOptions)).toHaveBeenCalledTimes(1);
    });

    act(() => {
      setDaemonConnection({
        baseUrl: "https://daemon-b.example",
        source: "test",
      });
    });

    await waitFor(() => {
      expect(vi.mocked(getProvidersBootstrap).mock.calls.length).toBeGreaterThan(1);
    });

    void requireHookValue(hookValue).ensureProviderAuthSummary("codex");
    await Promise.resolve();

    await waitFor(() => {
      expect(vi.mocked(getProviderOptions)).toHaveBeenCalledTimes(2);
    });

    pendingB.resolve({
      ...baseOptions("codex"),
      workspace_id: workspaceId,
      models: {
        models: [{ id: "gpt-5" }],
        current_model_id: "gpt-5",
      },
    });
    pendingA.resolve({
      ...baseOptions("codex"),
      workspace_id: workspaceId,
    });

    await waitFor(() => {
      expect(hookValue?.providerOptions.codex?.models).toEqual({
        models: [{ id: "gpt-5" }],
        current_model_id: "gpt-5",
      });
    });
  });

  it("selects provider install progress for the provider target", async () => {
    const workspaceId = "ws-target-install";
    let hookValue: HookValue | null = null;

    vi.mocked(getProviderInstallProgressSnapshotForScope).mockReturnValue({
      codex: {
        host: {
          installId: "install-host",
          state: "running",
          pct: 10,
          target: "host",
          errorCode: undefined,
          error: undefined,
          updatedAtMs: 1,
        },
        container: {
          installId: "install-container",
          state: "running",
          pct: 30,
          target: "container",
          errorCode: undefined,
          error: undefined,
          updatedAtMs: 2,
        },
      },
    });
    vi.mocked(getProvidersBootstrap).mockResolvedValue({
      ...makeBootstrap({
        codex: {
          ...baseOptions("codex"),
          workspace_id: workspaceId,
        },
      }),
      providers: [
        {
          provider_id: "codex",
          display_name: "Codex",
          installed: true,
          health: "ok",
          diagnostics: [],
          usability: {
            usable: true,
            status: "ready",
            blocking_provider_ids: [],
            recommended_action: "none",
          },
          details: {
            install_target: "container",
          },
        } as never,
      ],
    });

    render(createElement(WorkbenchProvidersHarness, {
      workspaceId,
      onChange: (next) => {
        hookValue = next;
      },
    }));

    await waitFor(() => {
      expect(hookValue?.providerInstallsById.codex?.installId).toBe("install-container");
      expect(hookValue?.providerInstallsById.codex?.target).toBe("container");
    });
  });

  it("applies extracted draft-harness replacement policy through the hook", async () => {
    const workspaceId = "ws-default-remap";
    let draftHarness: DraftHarness | null = null;
    let hookValue: HookValue | null = null;

    vi.mocked(getProvidersBootstrap).mockResolvedValue(makeBootstrap(
      {
        "claude-crp": {
          ...baseOptions("claude-crp"),
          workspace_id: workspaceId,
        },
      },
      [providerStatus("claude-crp")],
    ));

    render(createElement(WorkbenchProvidersDraftHarness, {
      workspaceId,
      initialDraftHarness: { providerId: "codex", modelId: "" },
      onChange: (next) => {
        hookValue = next.hookValue;
        draftHarness = next.draftHarness;
      },
    }));

    await waitFor(() => {
      expect(hookValue?.defaultProviderId).toBe("claude-crp");
      expect(draftHarness).toEqual({ providerId: "claude-crp", modelId: "" });
    });
  });

  it("starts only the requested provider updates from the warning-modal path", async () => {
    const workspaceId = "ws-provider-updates";
    let hookValue: HookValue | null = null;

    vi.mocked(getProvidersBootstrap).mockResolvedValue(makeBootstrap(
      {
        codex: {
          ...baseOptions("codex"),
          workspace_id: workspaceId,
        },
        "claude-crp": {
          ...baseOptions("claude-crp"),
          workspace_id: workspaceId,
        },
      },
      [
        providerStatus("codex", {
          details: {
            install_supported: "true",
            matrix_update_available: "true",
          },
        }),
        providerStatus("claude-crp", {
          details: {
            install_supported: "true",
            matrix_update_available: "true",
          },
        }),
      ],
    ));
    vi.mocked(installProvider)
      .mockResolvedValueOnce({
        provider_id: "codex",
        install_id: "install-codex",
        target: "host",
      })
      .mockResolvedValueOnce({
        provider_id: "claude-crp",
        install_id: "install-claude",
        target: "host",
      });

    render(createElement(WorkbenchProvidersHarness, {
      workspaceId,
      onChange: (next) => {
        hookValue = next;
      },
    }));

    await waitFor(() => {
      expect(hookValue?.bootstrapState).toBe("ready");
    });

    await act(async () => {
      await hookValue?.updateProvidersFromMenu(["codex", "claude-crp", "codex"]);
    });

    expect(vi.mocked(installProvider).mock.calls).toEqual([
      ["codex", "host"],
      ["claude-crp", "host"],
    ]);
    expect(vi.mocked(installAllProviders)).not.toHaveBeenCalled();
    expect(readAcknowledgedProviderRuntimeWarningIds(
      getProviderOwnerScopeKeyOrNull(workspaceId) ?? workspaceId,
    )).toEqual(["claude-crp", "codex"]);
  });

  it("acknowledges the current actionable warning set when installing all from the composer menu", async () => {
    const workspaceId = "ws-install-all";
    let hookValue: HookValue | null = null;

    vi.mocked(getProvidersBootstrap).mockResolvedValue(makeBootstrap(
      {
        codex: {
          ...baseOptions("codex"),
          workspace_id: workspaceId,
        },
        gemini: {
          ...baseOptions("gemini"),
          workspace_id: workspaceId,
        },
      },
      [
        providerStatus("codex", {
          details: {
            install_supported: "true",
            matrix_update_available: "true",
          },
        }),
        providerStatus("gemini", {
          installed: false,
          health: "missing",
          details: {
            install_supported: "true",
            matrix_update_available: "true",
          },
          usability: {
            usable: false,
            status: "blocked",
            blocking_provider_ids: [],
            recommended_action: "install",
            reason: "runtime not installed",
          },
        }),
      ],
    ));
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

    render(createElement(WorkbenchProvidersHarness, {
      workspaceId,
      onChange: (next) => {
        hookValue = next;
      },
    }));

    await waitFor(() => {
      expect(hookValue?.bootstrapState).toBe("ready");
    });

    await act(async () => {
      await hookValue?.installAllProvidersFromMenu();
    });

    expect(vi.mocked(installAllProviders)).toHaveBeenCalledWith("host");
    expect(readAcknowledgedProviderRuntimeWarningIds(
      getProviderOwnerScopeKeyOrNull(workspaceId) ?? workspaceId,
    )).toEqual(["codex"]);
  });
});
