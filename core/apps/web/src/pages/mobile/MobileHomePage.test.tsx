import type { Workspace } from "@ctx/types";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";

const listWorkspacesMock = vi.fn();

const buildConnection = (overrides?: Partial<{
  baseUrl: string | null;
  wsBaseUrl: string | null;
  authToken: string | null;
  runId: string | null;
  source: string | null;
  targetScope: { kind: string; baseUrl?: string | null } | null;
}>) => ({
  baseUrl: "https://daemon.example.com",
  wsBaseUrl: null,
  authToken: "mobile-token",
  runId: null,
  source: "mobile_manual_connect",
  targetScope: null,
  ...overrides,
});

let connection = buildConnection();

vi.mock("../../api/client", () => ({
  idToString: (value: unknown) => (typeof value === "string" && value.length > 0 ? value : null),
  listWorkspaces: (...args: unknown[]) => listWorkspacesMock(...args),
}));

vi.mock("../../api/useDaemonConnection", () => ({
  useDaemonConnection: () => connection,
}));

import { MobileHomePage } from "./MobileHomePage";

describe("MobileHomePage", () => {
  beforeEach(() => {
    connection = buildConnection();
    listWorkspacesMock.mockReset();
  });

  it("renders workspaces from the connected daemon and opens the selected workspace", async () => {
    const workspaces: Workspace[] = [
      {
        id: "workspace-1",
        name: "ctx-monorepo",
        root_path: "/home/fixture/code/ctx-monorepo",
        created_at: "2026-04-21T00:00:00Z",
        vcs_kind: "git",
      },
    ];
    listWorkspacesMock.mockResolvedValue(workspaces);

    render(
      <MemoryRouter initialEntries={["/"]}>
        <Routes>
          <Route path="/" element={<MobileHomePage />} />
          <Route path="/workspaces/:id" element={<div>Workspace opened</div>} />
        </Routes>
      </MemoryRouter>,
    );

    fireEvent.click(await screen.findByRole("button", { name: /ctx-monorepo/i }));

    expect(await screen.findByText("Workspace opened")).toBeInTheDocument();
  });

  it("refreshes the workspace list on demand", async () => {
    listWorkspacesMock.mockResolvedValue([]);

    render(
      <MemoryRouter initialEntries={["/"]}>
        <Routes>
          <Route path="/" element={<MobileHomePage />} />
        </Routes>
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(listWorkspacesMock).toHaveBeenCalledTimes(1);
    });

    fireEvent.click(screen.getByRole("button", { name: "Refresh workspaces" }));

    await waitFor(() => {
      expect(listWorkspacesMock).toHaveBeenCalledTimes(2);
    });
  });
});
