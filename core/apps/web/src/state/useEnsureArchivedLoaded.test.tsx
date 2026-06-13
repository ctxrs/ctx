import React from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";

import { useEnsureArchivedLoaded } from "./useEnsureArchivedLoaded";

type HarnessProps = {
  archivedCollapsed: boolean;
  archivedLoaded: boolean;
  fetchState: "idle" | "loading" | "error";
  activeInitialized: boolean;
  activeFetchState: "idle" | "loading" | "error";
  prefetchAfterActive?: boolean;
  ensureArchivedLoaded: () => void;
};

function Harness(props: HarnessProps) {
  useEnsureArchivedLoaded(props);
  return null;
}

afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
  cleanup();
});

beforeEach(() => {
  vi.useFakeTimers();
});

describe("useEnsureArchivedLoaded", () => {
  it("loads archived items when expanded and not loaded", () => {
    const ensureArchivedLoaded = vi.fn();
    render(
      <Harness
        archivedCollapsed={false}
        archivedLoaded={false}
        fetchState="idle"
        activeInitialized
        activeFetchState="idle"
        ensureArchivedLoaded={ensureArchivedLoaded}
      />,
    );
    act(() => {
      vi.advanceTimersByTime(300);
    });
    expect(ensureArchivedLoaded).toHaveBeenCalledTimes(1);
  });

  it("skips when collapsed or already loading", () => {
    const ensureArchivedLoaded = vi.fn();
    const { rerender } = render(
      <Harness
        archivedCollapsed
        archivedLoaded={false}
        fetchState="idle"
        activeInitialized
        activeFetchState="idle"
        ensureArchivedLoaded={ensureArchivedLoaded}
      />,
    );
    act(() => {
      vi.advanceTimersByTime(300);
    });
    rerender(
      <Harness
        archivedCollapsed={false}
        archivedLoaded={false}
        fetchState="loading"
        activeInitialized
        activeFetchState="idle"
        ensureArchivedLoaded={ensureArchivedLoaded}
      />,
    );
    act(() => {
      vi.advanceTimersByTime(300);
    });
    expect(ensureArchivedLoaded).toHaveBeenCalledTimes(0);
  });

  it("retries when expanded after an error", () => {
    const ensureArchivedLoaded = vi.fn();
    render(
      <Harness
        archivedCollapsed={false}
        archivedLoaded={false}
        fetchState="error"
        activeInitialized
        activeFetchState="idle"
        ensureArchivedLoaded={ensureArchivedLoaded}
      />,
    );
    act(() => {
      vi.advanceTimersByTime(300);
    });
    expect(ensureArchivedLoaded).toHaveBeenCalledTimes(1);
  });

  it("retries when the archived cache is cleared while expanded", () => {
    const ensureArchivedLoaded = vi.fn();
    const { rerender } = render(
      <Harness
        archivedCollapsed={false}
        archivedLoaded
        fetchState="idle"
        activeInitialized
        activeFetchState="idle"
        ensureArchivedLoaded={ensureArchivedLoaded}
      />,
    );
    act(() => {
      vi.advanceTimersByTime(300);
    });
    rerender(
      <Harness
        archivedCollapsed={false}
        archivedLoaded={false}
        fetchState="idle"
        activeInitialized
        activeFetchState="idle"
        ensureArchivedLoaded={ensureArchivedLoaded}
      />,
    );
    act(() => {
      vi.advanceTimersByTime(300);
    });
    expect(ensureArchivedLoaded).toHaveBeenCalledTimes(1);
  });
});
