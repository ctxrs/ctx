import { useEffect, useRef, useState } from "react";

/**
 * Coalesce rapid updates to at most once per animation frame.
 * Useful when downstream effects are expensive (e.g. imperative list sync).
 */
export function useRafCoalesced<T>(value: T): T {
  const latestRef = useRef(value);
  const [coalesced, setCoalesced] = useState(value);
  const rafRef = useRef<number | null>(null);
  latestRef.current = value;

  useEffect(() => {
    if (Object.is(value, coalesced)) return;
    if (rafRef.current != null) return;

    const schedule =
      typeof window !== "undefined" && typeof window.requestAnimationFrame === "function"
        ? window.requestAnimationFrame.bind(window)
        : (cb: FrameRequestCallback) => setTimeout(() => cb(Date.now()), 16);
    const cancel =
      typeof window !== "undefined" && typeof window.cancelAnimationFrame === "function"
        ? window.cancelAnimationFrame.bind(window)
        : clearTimeout;

    rafRef.current = schedule(() => {
      rafRef.current = null;
      setCoalesced(latestRef.current);
    }) as unknown as number;

    return () => {
      if (rafRef.current != null) {
        cancel(rafRef.current);
        rafRef.current = null;
      }
    };
  }, [value, coalesced]);

  return coalesced;
}
