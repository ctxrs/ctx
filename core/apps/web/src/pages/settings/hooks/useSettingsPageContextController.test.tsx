import type { ReactNode } from "react";
import { act, renderHook, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Workspace } from "../../../api/client";
import { listWorkspaces } from "../../../api/client";
import { useSettingsPageContextController } from "./useSettingsPageContextController";

vi.mock("../../../api/client", async () => {
  const actual = await vi.importActual<typeof import("../../../api/client")>("../../../api/client");
  return {
    ...actual,
    listWorkspaces: vi.fn(),
  };
});

const makeWorkspace = (id: string): Workspace => ({ id } as Workspace);

function wrapper(initialEntry: string) {
  return function MemoryRouterWrapper({ children }: { children: ReactNode }) {
    return <MemoryRouter initialEntries={[initialEntry]}>{children}</MemoryRouter>;
  };
}

describe("useSettingsPageContextController", () => {
  const listWorkspacesMock = vi.mocked(listWorkspaces);

  beforeEach(() => {
    listWorkspacesMock.mockReset();
    window.location.hash = "";
  });

  afterEach(() => {
    window.location.hash = "";
  });

  it("hydrates workspace selection from the url and clears checkout params", async () => {
    listWorkspacesMock.mockResolvedValue([makeWorkspace("workspace-a"), makeWorkspace("workspace-b")]);
    window.location.hash = "#billing";

    const { result } = renderHook(
      () => useSettingsPageContextController(),
      {
        wrapper: wrapper("/settings?ws=workspace-b&checkout=success&session_id=session-1"),
      },
    );

    await waitFor(() => {
      expect(result.current.workspaceId).toBe("workspace-b");
    });
    expect(result.current.headerLabel).toBe("General");
    expect(result.current.backLink).toEqual({
      to: "/workspaces/workspace-b",
      label: "← Back to Workspace",
    });
    expect(result.current.billingReturnPath).toBe("/settings?ws=workspace-b#general");
    expect(result.current.checkoutStatus).toBe("success");

    act(() => {
      result.current.clearCheckoutStatus();
    });

    await waitFor(() => {
      expect(result.current.checkoutStatus).toBeNull();
    });
  });

  it("maps legacy sandboxing hashes to the merged sandbox and networking page", async () => {
    listWorkspacesMock.mockResolvedValue([makeWorkspace("workspace-a")]);
    window.location.hash = "#sandboxing";

    const { result } = renderHook(
      () => useSettingsPageContextController(),
      {
        wrapper: wrapper("/settings"),
      },
    );

    await waitFor(() => {
      expect(result.current.workspaceId).toBe("workspace-a");
    });

    expect(result.current.active).toBe("container_network");
    expect(result.current.headerLabel).toBe("Sandbox & Networking");
  });

  it("filters sidebar sections and updates the hash when the active section changes", async () => {
    listWorkspacesMock.mockResolvedValue([makeWorkspace("workspace-a")]);

    const { result } = renderHook(
      () => useSettingsPageContextController(),
      {
        wrapper: wrapper("/settings"),
      },
    );

    await waitFor(() => {
      expect(result.current.workspaceId).toBe("workspace-a");
    });

    act(() => {
      result.current.setQuery("anal");
    });

    expect(result.current.sidebarSections.map((section) => section.label)).toEqual(["Analytics"]);

    act(() => {
      result.current.onSectionChange("analytics");
    });

    expect(result.current.active).toBe("analytics");
    expect(window.location.hash).toBe("#analytics");
  });
});
