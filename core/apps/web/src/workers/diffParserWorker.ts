/// <reference lib="webworker" />

import { parseUnifiedDiff } from "../components/diffReviewDiffParser";

self.addEventListener("message", (event: MessageEvent) => {
  const payload = (event.data ?? {}) as { id?: number; diff?: string };
  if (typeof payload.id !== "number") return;
  const files = parseUnifiedDiff(String(payload.diff ?? ""));
  (self as DedicatedWorkerGlobalScope).postMessage({ id: payload.id, files });
});
