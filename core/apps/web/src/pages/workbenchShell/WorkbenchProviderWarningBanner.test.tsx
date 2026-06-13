import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ProviderStatus } from "../../api/client";
import { WorkbenchProviderWarningBanner } from "./WorkbenchProviderWarningBanner";

const providerStatus = (
  providerId: string,
  overrides: Partial<ProviderStatus> = {},
): ProviderStatus => ({
  provider_id: providerId,
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
  ...overrides,
});

const deferred = () => {
  let resolve: (() => void) | null = null;
  let reject: ((error?: unknown) => void) | null = null;
  const promise = new Promise<void>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return {
    promise,
    resolve: () => resolve?.(),
    reject: (error?: unknown) => reject?.(error),
  };
};

describe("WorkbenchProviderWarningBanner", () => {
  beforeEach(() => {
    window.sessionStorage.clear();
  });

  it("renders nothing when no visible provider needs attention", () => {
    const { container } = render(
      <WorkbenchProviderWarningBanner
        acknowledgementScopeId="scope-ws-1"
        providersById={{ codex: providerStatus("codex") }}
        onUpdateProviders={() => Promise.resolve()}
        onOpenSettings={() => {}}
      />,
    );

    expect(container.firstChild).toBeNull();
  });

  it("uses the mobile placement class when rendered in the mobile shell", () => {
    render(
      <WorkbenchProviderWarningBanner
        acknowledgementScopeId="scope-ws-1"
        mobileShell
        providersById={{
          codex: providerStatus("codex", {
            details: {
              install_supported: "true",
              matrix_update_available: "true",
              matrix_recommended_version: "0.114.0-ctx.2",
            },
            version: "0.114.0-ctx.1",
          }),
        }}
        onUpdateProviders={() => Promise.resolve()}
        onOpenSettings={() => {}}
      />,
    );

    expect(screen.getByTestId("workbench-provider-warning")).toHaveClass("wb-provider-warning-snackbar-mobile");
  });

  it("surfaces all stale installed providers while omitting current installs from update actions", () => {
    const onUpdateProviders = vi.fn(() => Promise.resolve());

    render(
      <WorkbenchProviderWarningBanner
        acknowledgementScopeId="scope-ws-1"
        providersById={{
          codex: providerStatus("codex", {
            version: "0.114.0-ctx.3",
            health: "unsupported_version",
            details: {
              install_supported: "true",
              matrix_update_available: "true",
              matrix_recommended_version: "0.114.0-ctx.5",
            },
          }),
          gemini: providerStatus("gemini", {
            version: "0.33.1",
            health: "unsupported_version",
            details: {
              install_supported: "true",
              matrix_update_available: "true",
              matrix_recommended_version: "0.38.2",
            },
          }),
          cursor: providerStatus("cursor", {
            version: "0.7.1",
          }),
        }}
        onUpdateProviders={onUpdateProviders}
        onOpenSettings={() => {}}
      />,
    );

    expect(screen.getByText("2 provider runtimes need an update.")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Update All" }));
    expect(onUpdateProviders).toHaveBeenCalledWith(["codex", "gemini"]);
  });

  it("acknowledges the current provider set on update all and keeps the notice hidden until that set fully clears", async () => {
    const update = deferred();
    const onUpdateProviders = vi.fn(() => update.promise);
    const onOpenSettings = vi.fn();

    const { rerender } = render(
      <WorkbenchProviderWarningBanner
        acknowledgementScopeId="scope-ws-1"
        providersById={{
          codex: providerStatus("codex", {
            details: {
              install_supported: "true",
              matrix_update_available: "true",
              matrix_recommended_version: "0.114.0-ctx.2",
            },
            version: "0.114.0-ctx.1",
          }),
          "claude-crp": providerStatus("claude-crp", {
            details: {
              install_supported: "true",
              matrix_update_available: "true",
              matrix_recommended_version: "1.2.4",
            },
            version: "1.2.3",
          }),
        }}
        updateAllBusy={false}
        onUpdateProviders={onUpdateProviders}
        onOpenSettings={onOpenSettings}
      />,
    );

    expect(screen.getByTestId("workbench-provider-warning")).toBeInTheDocument();
    expect(screen.getByText("2 provider runtimes need an update.")).toBeInTheDocument();
    expect(screen.queryByText("Codex")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Update All" }));
    expect(onUpdateProviders).toHaveBeenCalledWith(["claude-crp", "codex"]);

    await waitFor(() => {
      expect(screen.queryByTestId("workbench-provider-warning")).not.toBeInTheDocument();
    });

    rerender(
      <WorkbenchProviderWarningBanner
        acknowledgementScopeId="scope-ws-1"
        providersById={{
          "claude-crp": providerStatus("claude-crp", {
            details: {
              install_supported: "true",
              matrix_update_available: "true",
              matrix_recommended_version: "1.2.4",
            },
            version: "1.2.3",
          }),
        }}
        updateAllBusy={true}
        onUpdateProviders={onUpdateProviders}
        onOpenSettings={onOpenSettings}
      />,
    );

    expect(screen.queryByTestId("workbench-provider-warning")).not.toBeInTheDocument();

    update.resolve();
    rerender(
      <WorkbenchProviderWarningBanner
        acknowledgementScopeId="scope-ws-1"
        providersById={{
          "claude-crp": providerStatus("claude-crp", {
            details: {
              install_supported: "true",
              matrix_update_available: "true",
              matrix_recommended_version: "1.2.4",
            },
            version: "1.2.3",
          }),
        }}
        updateAllBusy={false}
        onUpdateProviders={onUpdateProviders}
        onOpenSettings={onOpenSettings}
      />,
    );

    await waitFor(() => {
      expect(screen.queryByTestId("workbench-provider-warning")).not.toBeInTheDocument();
    });

    rerender(
      <WorkbenchProviderWarningBanner
        acknowledgementScopeId="scope-ws-1"
        providersById={{}}
        updateAllBusy={false}
        onUpdateProviders={onUpdateProviders}
        onOpenSettings={onOpenSettings}
      />,
    );

    await waitFor(() => {
      expect(screen.queryByTestId("workbench-provider-warning")).not.toBeInTheDocument();
    });

    rerender(
      <WorkbenchProviderWarningBanner
        acknowledgementScopeId="scope-ws-1"
        providersById={{
          "claude-crp": providerStatus("claude-crp", {
            details: {
              install_supported: "true",
              matrix_update_available: "true",
              matrix_recommended_version: "1.2.4",
            },
            version: "1.2.3",
          }),
        }}
        updateAllBusy={false}
        onUpdateProviders={onUpdateProviders}
        onOpenSettings={onOpenSettings}
      />,
    );

    await waitFor(() => {
      expect(screen.getByText("1 provider runtime needs an update.")).toBeInTheDocument();
    });
    expect(onOpenSettings).not.toHaveBeenCalled();
  });

  it("dismisses before opening settings and keeps the warning hidden while only the acknowledged provider set is flagged", async () => {
    const onOpenSettings = vi.fn();

    const { rerender } = render(
      <WorkbenchProviderWarningBanner
        acknowledgementScopeId="scope-ws-1"
        providersById={{
          codex: providerStatus("codex", {
            details: {
              install_supported: "true",
              matrix_update_available: "true",
            },
          }),
          "claude-crp": providerStatus("claude-crp", {
            health: "unsupported_version",
            details: { matrix_update_available: "true" },
            diagnostics: ["Provider version requires a newer ctx build"],
          }),
        }}
        onUpdateProviders={() => Promise.resolve()}
        onOpenSettings={onOpenSettings}
      />,
    );

    expect(screen.getByRole("button", { name: "Update All" })).toBeInTheDocument();
    expect(screen.getByText("2 provider runtimes need an update.")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Open Settings" }));
    expect(onOpenSettings).toHaveBeenCalledTimes(1);

    await waitFor(() => {
      expect(screen.queryByTestId("workbench-provider-warning")).not.toBeInTheDocument();
    });

    rerender(
      <WorkbenchProviderWarningBanner
        acknowledgementScopeId="scope-ws-1"
        providersById={{
          "claude-crp": providerStatus("claude-crp", {
            health: "unsupported_version",
            details: { matrix_update_available: "true" },
            diagnostics: ["Provider version requires a newer ctx build"],
          }),
        }}
        onUpdateProviders={() => Promise.resolve()}
        onOpenSettings={onOpenSettings}
      />,
    );

    expect(screen.queryByTestId("workbench-provider-warning")).not.toBeInTheDocument();

    rerender(
      <WorkbenchProviderWarningBanner
        acknowledgementScopeId="scope-ws-1"
        providersById={{
          "claude-crp": providerStatus("claude-crp", {
            health: "unsupported_version",
            details: { matrix_update_available: "true" },
            diagnostics: ["Provider version requires a newer ctx build"],
          }),
          gemini: providerStatus("gemini", {
            details: {
              install_supported: "true",
              matrix_update_available: "true",
            },
          }),
        }}
        onUpdateProviders={() => Promise.resolve()}
        onOpenSettings={onOpenSettings}
      />,
    );

    expect(screen.getByTestId("workbench-provider-warning")).toBeInTheDocument();
  });

  it("renders again after the acknowledged provider set fully clears", async () => {
    const { rerender } = render(
      <WorkbenchProviderWarningBanner
        acknowledgementScopeId="scope-ws-1"
        providersById={{
          codex: providerStatus("codex", {
            details: { matrix_update_available: "true" },
          }),
        }}
        onUpdateProviders={() => Promise.resolve()}
        onOpenSettings={() => {}}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    await waitFor(() => {
      expect(screen.queryByTestId("workbench-provider-warning")).not.toBeInTheDocument();
    });

    rerender(
      <WorkbenchProviderWarningBanner
        acknowledgementScopeId="scope-ws-1"
        providersById={{}}
        onUpdateProviders={() => Promise.resolve()}
        onOpenSettings={() => {}}
      />,
    );

    await waitFor(() => {
      expect(screen.queryByTestId("workbench-provider-warning")).not.toBeInTheDocument();
    });

    rerender(
      <WorkbenchProviderWarningBanner
        acknowledgementScopeId="scope-ws-1"
        providersById={{
          codex: providerStatus("codex", {
            details: {
              install_supported: "true",
              matrix_update_available: "true",
            },
            version: "0.114.0-ctx.1",
          }),
        }}
        onUpdateProviders={() => Promise.resolve()}
        onOpenSettings={() => {}}
      />,
    );

    expect(screen.getByTestId("workbench-provider-warning")).toBeInTheDocument();
  });

  it("falls back to settings-only when no flagged provider supports managed updates", () => {
    render(
      <WorkbenchProviderWarningBanner
        acknowledgementScopeId="scope-ws-1"
        providersById={{
          "claude-crp": providerStatus("claude-crp", {
            health: "unsupported_version",
            details: { matrix_update_available: "true" },
            diagnostics: ["Provider version requires a newer ctx build"],
          }),
        }}
        onUpdateProviders={() => Promise.resolve()}
        onOpenSettings={() => {}}
      />,
    );

    expect(screen.queryByRole("button", { name: "Update All" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open Settings" })).toBeInTheDocument();
    expect(screen.getByText("1 provider runtime needs an update.")).toBeInTheDocument();
  });

  it("does not render on a clean workspace launch when only never-installed providers are flagged", () => {
    const { container } = render(
      <WorkbenchProviderWarningBanner
        acknowledgementScopeId="scope-clean-workspace"
        providersById={{
          droid: providerStatus("droid", {
            installed: false,
            health: "error",
            details: {
              install_supported: "true",
              managed_fingerprint_mismatch: "true",
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
          gemini: providerStatus("gemini", {
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
        }}
        onUpdateProviders={() => Promise.resolve()}
        onOpenSettings={() => {}}
      />,
    );

    expect(container.firstChild).toBeNull();
  });
});
