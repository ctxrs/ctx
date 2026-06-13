import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  BROWSER_CAPABILITY_REFRESH_MARGIN_MS,
  BROWSER_CAPABILITY_TOKEN_TTL_MS,
} from "../api/browserCapabilityAuth";
import {
  resetDaemonConnectionStateForTests,
  setDaemonConnection,
} from "../api/daemonConnection";
import { resetBrowserResourceUrlCacheForTests } from "../api/browserResourceUrls";
import { MessageAttachmentImage } from "./MessageAttachmentImage";

describe("MessageAttachmentImage", () => {
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
    resetBrowserResourceUrlCacheForTests();
    resetDaemonConnectionStateForTests();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("keeps blob-backed image src stable across rerenders", () => {
    const nowSpy = vi.spyOn(Date, "now").mockReturnValue(1_761_600_000_000);
    const rendered = render(
      <MessageAttachmentImage
        attachment={{
          kind: "image_ref",
          blob_id: "blob-1",
          mime_type: "image/png",
          name: "sample.png",
        }}
        className="attachment-img"
        alt="sample"
      />,
    );
    const image = rendered.container.querySelector("img");
    const src = image?.getAttribute("src");

    nowSpy.mockReturnValue(1_761_600_002_000);
    rendered.rerender(
      <MessageAttachmentImage
        attachment={{
          kind: "image_ref",
          blob_id: "blob-1",
          mime_type: "image/png",
          name: "sample.png",
        }}
        className="attachment-img"
        alt="sample"
      />,
    );

    expect(rendered.container.querySelector("img")?.getAttribute("src")).toBe(src);
  });

  it("refreshes a mounted blob-backed image src near capability expiry", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(1_761_600_000_000));
    const rendered = render(
      <MessageAttachmentImage
        attachment={{
          kind: "image_ref",
          blob_id: "blob-1",
          mime_type: "image/png",
          name: "sample.png",
        }}
        className="attachment-img"
        alt="sample"
      />,
    );
    const firstSrc = rendered.container.querySelector("img")?.getAttribute("src");

    act(() => {
      vi.advanceTimersByTime(BROWSER_CAPABILITY_TOKEN_TTL_MS - BROWSER_CAPABILITY_REFRESH_MARGIN_MS + 1);
    });

    expect(rendered.container.querySelector("img")?.getAttribute("src")).not.toBe(firstSrc);
  });

  it("renders an explicit unsupported state when no browser resource token is available", () => {
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
      <MessageAttachmentImage
        attachment={{
          kind: "image_ref",
          blob_id: "blob-1",
          mime_type: "image/png",
          name: "sample.png",
        }}
        className="attachment-img"
        alt="sample"
      />,
    );

    expect(screen.getByText("Image unavailable")).toBeInTheDocument();
  });
});
