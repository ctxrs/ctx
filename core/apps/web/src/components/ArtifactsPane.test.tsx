import React from "react";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Artifact } from "../api/client";
import { resetBrowserResourceUrlCacheForTests } from "../api/browserResourceUrls";
import {
  resetDaemonConnectionStateForTests,
  setDaemonConnection,
} from "../api/daemonConnection";
import { ArtifactsPane } from "./ArtifactsPane";

const makeArtifact = (overrides: Partial<Artifact> = {}): Artifact =>
  ({
    id: "artifact-1",
    session_id: "session-1",
    task_id: "task-1",
    workspace_id: "workspace-1",
    worktree_id: "worktree-1",
    turn_id: null,
    name: "artifact",
    mime_type: "text/plain",
    bytes: 128,
    absolute_path: "/tmp/artifact.txt",
    missing: false,
    created_at: "2026-03-13T00:00:00.000Z",
    ...overrides,
  }) as Artifact;

const originalFetch = global.fetch;

function mockTextFetch(opts: { ok?: boolean; status?: number; text?: string } = {}) {
  const response = {
    ok: opts.ok ?? true,
    status: opts.status ?? 200,
    headers: {
      get: (key: string) => key.toLowerCase() === "content-type" ? "text/plain" : null,
    },
    text: async () => opts.text ?? "",
  } as Response;
  global.fetch = vi.fn(async () => response);
}

const sampleMdx = `---
title: Merge queue for agents
description: Fast path
---

import Example from "./Example"

## Inline Heading

Preview body text.

<p style={{ color: "red" }}>This should not render literally.</p>
`;

beforeEach(() => {
  resetBrowserResourceUrlCacheForTests();
  setDaemonConnection({
    baseUrl: "http://daemon.test",
    authToken: "daemon-secret",
    source: "test",
    mobileSecure: null,
  });
});

afterEach(() => {
  global.fetch = originalFetch;
  resetBrowserResourceUrlCacheForTests();
  resetDaemonConnectionStateForTests();
  vi.restoreAllMocks();
});

function extractTransformNumber(transform: string, name: "translate" | "scale", axis?: "x" | "y"): number {
  if (name === "scale") {
    const match = /scale\(([-+\d.]+)\)/.exec(transform);
    return match ? Number(match[1]) : Number.NaN;
  }
  const match = /translate\(([-+\d.]+)px(?:,\s*|\s+)([-+\d.]+)px\)/.exec(transform);
  if (!match) return Number.NaN;
  return Number(axis === "y" ? match[2] : match[1]);
}

function openImageViewer() {
  render(
    <ArtifactsPane
      sessionId="session-1"
      artifacts={[
        makeArtifact({
          name: "sample.png",
          absolute_path: "/tmp/sample.png",
          mime_type: "image/png",
          bytes: 2048,
        }),
      ]}
    />,
  );
  fireEvent.click(screen.getByText("sample.png"));
  const body = document.querySelector(".wb-artifact-modal-body") as HTMLDivElement;
  const image = document.querySelector(".wb-artifact-modal-image") as HTMLImageElement;
  Object.defineProperty(body, "clientWidth", { configurable: true, value: 300 });
  Object.defineProperty(body, "clientHeight", { configurable: true, value: 200 });
  body.getBoundingClientRect = () =>
    ({
      x: 0,
      y: 0,
      left: 0,
      top: 0,
      right: 300,
      bottom: 200,
      width: 300,
      height: 200,
      toJSON: () => ({}),
    }) as DOMRect;
  Object.defineProperty(image, "clientWidth", { configurable: true, value: 200 });
  Object.defineProperty(image, "clientHeight", { configurable: true, value: 100 });
  fireEvent.load(image);
  return { body, image };
}

describe("ArtifactsPane", () => {
  it("renders an explicit load error with retry affordance", () => {
    const onRetry = vi.fn();

    render(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[]}
        error="Failed to load artifacts: daemon offline"
        onRetry={onRetry}
      />,
    );

    expect(screen.getByRole("alert")).toHaveTextContent("Failed to load artifacts: daemon offline");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("keeps the existing empty state when there is no load error", () => {
    render(<ArtifactsPane sessionId="session-1" artifacts={[]} />);
    expect(screen.getByText("No artifacts yet.")).toBeInTheDocument();
  });

  it("opens the viewer for previewable artifacts", () => {
    render(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[
          makeArtifact({
            name: "chart.png",
            mime_type: "image/png",
            absolute_path: "/tmp/chart.png",
          }),
        ]}
      />,
    );

    fireEvent.click(screen.getByTitle("/tmp/chart.png"));

    expect(screen.getByRole("button", { name: "Close" })).toBeInTheDocument();
  });

  it("closes the viewer when the session changes", () => {
    const { rerender } = render(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[
          makeArtifact({
            name: "chart.png",
            mime_type: "image/png",
            absolute_path: "/tmp/chart.png",
          }),
        ]}
      />,
    );

    fireEvent.click(screen.getByTitle("/tmp/chart.png"));
    expect(screen.getByRole("button", { name: "Close" })).toBeInTheDocument();

    rerender(
      <ArtifactsPane
        sessionId="session-2"
        artifacts={[
          makeArtifact({
            id: "artifact-2",
            session_id: "session-2",
            name: "other.png",
            mime_type: "image/png",
            absolute_path: "/tmp/other.png",
          }),
        ]}
      />,
    );

    expect(screen.queryByRole("button", { name: "Close" })).not.toBeInTheDocument();
  });

  it("closes the viewer when a same-session refresh removes the open artifact", () => {
    const { rerender } = render(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[
          makeArtifact({
            id: "artifact-1",
            session_id: "session-1",
            name: "chart.png",
            mime_type: "image/png",
            absolute_path: "/tmp/chart.png",
          }),
        ]}
      />,
    );

    fireEvent.click(screen.getByTitle("/tmp/chart.png"));
    expect(screen.getByRole("button", { name: "Close" })).toBeInTheDocument();

    rerender(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[
          makeArtifact({
            id: "artifact-2",
            session_id: "session-1",
            name: "other.png",
            mime_type: "image/png",
            absolute_path: "/tmp/other.png",
          }),
        ]}
      />,
    );

    expect(screen.queryByRole("button", { name: "Close" })).not.toBeInTheDocument();
  });

  it("does not open the viewer for unsupported artifacts", () => {
    render(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[
          makeArtifact({
            name: "report.csv",
            mime_type: "text/csv",
            absolute_path: "/tmp/report.csv",
          }),
        ]}
      />,
    );

    fireEvent.click(screen.getByTitle("/tmp/report.csv"));

    expect(screen.queryByRole("button", { name: "Close" })).not.toBeInTheDocument();
  });

  it("autoplays previewable video artifacts inline and in the viewer", () => {
    render(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[
          makeArtifact({
            name: "demo.mp4",
            mime_type: "video/mp4",
            absolute_path: "/tmp/demo.mp4",
          }),
        ]}
      />,
    );

    const inlineVideo = document.querySelector(".wb-artifact-video") as HTMLVideoElement;
    expect(inlineVideo).toBeTruthy();
    expect(inlineVideo.autoplay).toBe(true);
    expect(inlineVideo.loop).toBe(true);
    expect(inlineVideo.muted).toBe(true);
    expect(inlineVideo.playsInline).toBe(true);
    fireEvent.pointerDown(inlineVideo);
    fireEvent.click(inlineVideo);
    expect(screen.queryByRole("button", { name: "Close" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByTitle("/tmp/demo.mp4"));

    const modalVideo = document.querySelector(".wb-artifact-modal-video") as HTMLVideoElement;
    expect(modalVideo).toBeTruthy();
    expect(modalVideo.autoplay).toBe(true);
    expect(modalVideo.loop).toBe(true);
    expect(modalVideo.muted).toBe(true);
    expect(modalVideo.playsInline).toBe(true);
  });

  it("renders an inline text preview for previewable text artifacts", async () => {
    mockTextFetch({ text: "line 1\nline 2\nline 3" });

    render(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[
          makeArtifact({
            name: "notes.txt",
            mime_type: "text/plain",
            absolute_path: "/tmp/notes.txt",
          }),
        ]}
      />,
    );

    expect(await screen.findByText(/line 2/, { selector: "pre" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Close" })).not.toBeInTheDocument();
  });

  it("renders markdown artifacts in the viewer", async () => {
    mockTextFetch({ text: "# Heading\n\nSome artifact text." });

    render(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[
          makeArtifact({
            name: "notes.md",
            mime_type: "text/markdown",
            absolute_path: "/tmp/notes.md",
          }),
        ]}
      />,
    );

    fireEvent.click(screen.getByTitle("/tmp/notes.md"));

    const headings = await screen.findAllByText("Heading");
    expect(headings).toHaveLength(2);
    const bodyText = screen.getAllByText("Some artifact text.");
    expect(bodyText).toHaveLength(2);
  });

  it("renders markdown artifacts as markdown in the inline preview", async () => {
    mockTextFetch({ text: "## Inline Heading\n\nPreview body text." });

    render(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[
          makeArtifact({
            name: "preview.md",
            mime_type: "text/markdown",
            absolute_path: "/tmp/preview.md",
          }),
        ]}
      />,
    );

    expect(await screen.findByText("Inline Heading")).toBeInTheDocument();
    expect(screen.getByText("Preview body text.")).toBeInTheDocument();
    expect(screen.queryByText("## Inline Heading")).not.toBeInTheDocument();
  });

  it("keeps inline text preview content stable across unrelated rerenders", async () => {
    const nowSpy = vi.spyOn(Date, "now").mockReturnValue(1_761_600_000_000);
    mockTextFetch({ text: "Stable preview body." });

    const rendered = render(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[
          makeArtifact({
            name: "preview.txt",
            mime_type: "text/plain",
            absolute_path: "/tmp/preview.txt",
          }),
        ]}
      />,
    );

    expect(await screen.findByText("Stable preview body.")).toBeInTheDocument();
    expect(global.fetch).toHaveBeenCalledTimes(1);

    nowSpy.mockReturnValue(1_761_600_002_000);
    rendered.rerender(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[
          makeArtifact({
            name: "preview.txt",
            mime_type: "text/plain",
            absolute_path: "/tmp/preview.txt",
          }),
        ]}
      />,
    );

    await waitFor(() => expect(global.fetch).toHaveBeenCalledTimes(1));
    expect(screen.getByText("Stable preview body.")).toBeInTheDocument();
    expect(screen.queryByText("Loading preview…")).not.toBeInTheDocument();
  });

  it("keeps modal text preview content stable across unrelated rerenders", async () => {
    const nowSpy = vi.spyOn(Date, "now").mockReturnValue(1_761_600_000_000);
    mockTextFetch({ text: "Modal preview body." });

    const artifact = makeArtifact({
      name: "modal.txt",
      mime_type: "text/plain",
      absolute_path: "/tmp/modal.txt",
    });
    const rendered = render(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[artifact]}
      />,
    );

    expect(await screen.findByText("Modal preview body.")).toBeInTheDocument();
    fireEvent.click(screen.getByTitle("/tmp/modal.txt"));
    await waitFor(() => expect(screen.getAllByText("Modal preview body.")).toHaveLength(2));
    expect(global.fetch).toHaveBeenCalledTimes(2);

    nowSpy.mockReturnValue(1_761_600_002_000);
    rendered.rerender(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[artifact]}
      />,
    );

    await waitFor(() => expect(global.fetch).toHaveBeenCalledTimes(2));
    expect(screen.getAllByText("Modal preview body.")).toHaveLength(2);
    expect(screen.queryByText("Loading artifact…")).not.toBeInTheDocument();
  });

  it("keeps artifact image URLs stable across rerenders", () => {
    const nowSpy = vi.spyOn(Date, "now").mockReturnValue(1_761_600_000_000);
    const artifact = makeArtifact({
      name: "sample.png",
      absolute_path: "/tmp/sample.png",
      mime_type: "image/png",
      bytes: 2048,
    });
    const rendered = render(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[artifact]}
      />,
    );
    const image = document.querySelector(".wb-artifact-image") as HTMLImageElement;
    const src = image.getAttribute("src");

    nowSpy.mockReturnValue(1_761_600_002_000);
    rendered.rerender(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[artifact]}
      />,
    );

    const nextImage = document.querySelector(".wb-artifact-image") as HTMLImageElement;
    expect(nextImage.getAttribute("src")).toBe(src);
  });

  it("shows an explicit unsupported state for image artifacts without a browser resource token", () => {
    resetBrowserResourceUrlCacheForTests();
    setDaemonConnection({
      baseUrl: "http://daemon.test",
      authToken: null,
      source: "test",
      mobileSecure: {
        kind: "managed_tunnel",
        deviceId: "device-1",
        daemonPublicKey: "public-key",
        pairingRequestEncryption: "pairing",
        nextSeq: 1,
      },
    });

    render(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[
          makeArtifact({
            name: "sample.png",
            absolute_path: "/tmp/sample.png",
            mime_type: "image/png",
            bytes: 2048,
          }),
        ]}
      />,
    );

    expect(screen.getByText("Resource preview is unavailable for this connection.")).toBeInTheDocument();
    fireEvent.click(screen.getByText("sample.png"));
    expect(screen.getByRole("alert")).toHaveTextContent("Resource preview is unavailable for this connection.");
  });

  it("normalizes mdx artifacts in inline and modal previews", async () => {
    mockTextFetch({ text: sampleMdx });

    render(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[
          makeArtifact({
            name: "merge-queue-for-agents.mdx",
            mime_type: "application/octet-stream",
            absolute_path: "/tmp/merge-queue-for-agents.mdx",
          }),
        ]}
      />,
    );

    expect(await screen.findByText("Inline Heading")).toBeInTheDocument();
    expect(screen.getAllByText("Preview body text.")).toHaveLength(1);
    expect(screen.queryByText(/title:/)).not.toBeInTheDocument();
    expect(screen.queryByText(/<p style=/)).not.toBeInTheDocument();

    fireEvent.click(screen.getByTitle("/tmp/merge-queue-for-agents.mdx"));

    await waitFor(() => {
      expect(screen.getAllByText("Inline Heading")).toHaveLength(2);
      expect(screen.getAllByText("Preview body text.")).toHaveLength(2);
    });
    expect(screen.queryByText(/title:/)).not.toBeInTheDocument();
    expect(screen.queryByText(/<p style=/)).not.toBeInTheDocument();
  });

  it("renders json artifacts as text in the viewer", async () => {
    mockTextFetch({ text: '{\n  "ok": true\n}' });

    render(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[
          makeArtifact({
            name: "report.json",
            mime_type: "application/json",
            absolute_path: "/tmp/report.json",
          }),
        ]}
      />,
    );

    fireEvent.click(screen.getByTitle("/tmp/report.json"));

    const jsonPreviews = await screen.findAllByText(/"ok": true/, { selector: "pre" });
    expect(jsonPreviews).toHaveLength(2);
  });

  it("shows an inline error when text artifact loading fails", async () => {
    mockTextFetch({ ok: false, status: 500 });

    render(
      <ArtifactsPane
        sessionId="session-1"
        artifacts={[
          makeArtifact({
            name: "broken.txt",
            mime_type: "text/plain",
            absolute_path: "/tmp/broken.txt",
          }),
        ]}
      />,
    );

    fireEvent.click(screen.getByTitle("/tmp/broken.txt"));

    expect(await screen.findByRole("alert")).toHaveTextContent("Failed to load artifact (500).");
  });

  it("keeps wheel zoom changes bounded for image artifacts", () => {
    const { body, image } = openImageViewer();

    fireEvent.wheel(body, { deltaY: -400, clientX: 150, clientY: 100 });

    expect(extractTransformNumber(image.style.transform, "scale")).toBeLessThan(1.2);
    expect(extractTransformNumber(image.style.transform, "scale")).toBeGreaterThan(1);
  });

  it("keeps drag transforms finite while zoomed", async () => {
    const { body, image } = openImageViewer();

    Object.defineProperty(body, "setPointerCapture", { configurable: true, value: vi.fn() });
    Object.defineProperty(body, "releasePointerCapture", { configurable: true, value: vi.fn() });
    Object.defineProperty(body, "hasPointerCapture", { configurable: true, value: vi.fn(() => true) });

    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    await waitFor(() => {
      expect(extractTransformNumber(image.style.transform, "scale")).toBeGreaterThan(1.7);
    });

    fireEvent.pointerDown(body, { pointerId: 1, clientX: 100, clientY: 100 });
    expect(body).toHaveClass("wb-artifact-dragging");

    fireEvent.pointerMove(body, { pointerId: 1, clientX: 260, clientY: 100 });
    await waitFor(() => {
      expect(image.style.transform).not.toContain("NaN");
      expect(Number.isFinite(extractTransformNumber(image.style.transform, "translate", "x"))).toBe(true);
      expect(Number.isFinite(extractTransformNumber(image.style.transform, "translate", "y"))).toBe(true);
    });

    fireEvent.pointerUp(body, { pointerId: 1, clientX: 260, clientY: 100 });
    expect(body).not.toHaveClass("wb-artifact-dragging");
  });
});
