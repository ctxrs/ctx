import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { SessionsPane } from "./SessionsPane";
import { mintWebSessionStreamPath, type WebSessionInfo } from "../api/client";

const useDaemonConnectionMock = vi.hoisted(() => vi.fn());

vi.mock("../api/client", async (importOriginal) => {
  const original = await importOriginal<typeof import("../api/client")>();
  return {
    ...original,
    mintWebSessionStreamPath: vi.fn(),
  };
});

vi.mock("../api/useDaemonConnection", () => ({
  useDaemonConnection: useDaemonConnectionMock,
}));

function makeSession(id: string, url: string): WebSessionInfo {
  return {
    id,
    kind: "web",
    session_id: "session-1",
    worktree_id: "worktree-1",
    status: "running",
    created_at: "2026-04-26T00:00:00Z",
    updated_at: "2026-04-26T00:00:00Z",
    last_activity: "2026-04-26T00:00:00Z",
    url,
    viewport: { width: 1280, height: 720 },
    fps: 30,
    viewers: 0,
    stream_path: `/sessions/web/${id}/view`,
    stream_url: null,
  };
}

describe("SessionsPane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useDaemonConnectionMock.mockReturnValue({
      baseUrl: "http://127.0.0.1:4399",
      wsBaseUrl: "ws://127.0.0.1:4399",
      authToken: null,
      runId: null,
      source: "desktop",
      targetScope: { kind: "desktop_local" },
    });
  });

  it("mints a fresh stream URL for the selected session", async () => {
    vi.mocked(mintWebSessionStreamPath).mockResolvedValue({
      stream_path: "/sessions/web/web-1/view?token=fresh-token",
      stream_url: null,
      expires_at: "2026-04-26T00:00:30Z",
    });

    render(
      <SessionsPane
        sections={[
          {
            key: "web",
            label: "Web Sessions",
            sessions: [makeSession("web-1", "https://example.com/path")],
          },
        ]}
        activeSection="web"
        onSectionChange={vi.fn()}
        selectedSessionId="web-1"
        onSelectSession={vi.fn()}
        daemonBaseUrl="http://127.0.0.1:4399"
      />,
    );

    await waitFor(() => {
      expect(mintWebSessionStreamPath).toHaveBeenCalledWith("web-1");
    });

    const frame = await screen.findByTitle("session-web-1");
    expect(frame).toHaveAttribute(
      "src",
      "http://127.0.0.1:4399/sessions/web/web-1/view?token=fresh-token",
    );
  });

  it("retries stream mint after desktop auth arrives later", async () => {
    vi.mocked(mintWebSessionStreamPath)
      .mockRejectedValueOnce(new Error("unauthorized"))
      .mockResolvedValueOnce({
        stream_path: "/sessions/web/web-1/view?token=fresh-token",
        stream_url: null,
        expires_at: "2026-04-26T00:00:30Z",
      });

    const props = {
      sections: [
        {
          key: "web",
          label: "Web Sessions",
          sessions: [makeSession("web-1", "https://example.com/path")],
        },
      ],
      activeSection: "web",
      onSectionChange: vi.fn(),
      selectedSessionId: "web-1",
      onSelectSession: vi.fn(),
      daemonBaseUrl: "http://127.0.0.1:4399",
    };

    const { rerender } = render(<SessionsPane {...props} />);

    await waitFor(() => {
      expect(mintWebSessionStreamPath).toHaveBeenCalledTimes(1);
    });
    expect(screen.getByText("Stream unavailable.")).toBeInTheDocument();

    useDaemonConnectionMock.mockReturnValue({
      baseUrl: "http://127.0.0.1:4399",
      wsBaseUrl: "ws://127.0.0.1:4399",
      authToken: "desktop-token",
      runId: null,
      source: "desktop",
      targetScope: { kind: "desktop_local" },
    });

    rerender(<SessionsPane {...props} />);

    await waitFor(() => {
      expect(mintWebSessionStreamPath).toHaveBeenCalledTimes(2);
    });
    const frame = await screen.findByTitle("session-web-1");
    expect(frame).toHaveAttribute(
      "src",
      "http://127.0.0.1:4399/sessions/web/web-1/view?token=fresh-token",
    );
  });
});
