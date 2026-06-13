import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useRelativeNowMs } from "./useRelativeNowMs";

function Harness({ intervalMs = 60_000 }: { intervalMs?: number }) {
  const nowMs = useRelativeNowMs(intervalMs);
  return <div data-testid="now">{nowMs}</div>;
}

describe("useRelativeNowMs", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2025-01-01T00:00:00.000Z"));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("ticks on the configured interval", () => {
    const start = Date.now();
    render(<Harness intervalMs={1000} />);
    expect(screen.getByTestId("now").textContent).toBe(String(start));

    act(() => {
      vi.advanceTimersByTime(1000);
    });

    expect(screen.getByTestId("now").textContent).toBe(String(start + 1000));
  });

  it("refreshes on visibility change", () => {
    const start = Date.now();
    render(<Harness intervalMs={60_000} />);
    expect(screen.getByTestId("now").textContent).toBe(String(start));

    act(() => {
      vi.setSystemTime(new Date(start + 30_000));
      document.dispatchEvent(new Event("visibilitychange"));
    });

    expect(screen.getByTestId("now").textContent).toBe(String(start + 30_000));
  });
});
