export type FlowRunToken = {
  runId: number;
  targetKey: string;
};

export const nextFlowRunToken = (
  currentRunId: number,
  targetKey: string,
): FlowRunToken => ({
  runId: currentRunId + 1,
  targetKey,
});

export const isCurrentFlowRunToken = (
  active: FlowRunToken | null,
  candidate: FlowRunToken,
): boolean => Boolean(active)
  && active!.runId === candidate.runId
  && active!.targetKey === candidate.targetKey;

export const clampStepKey = (
  stepKeys: string[],
  currentKey: string,
  fallbackIndex = 0,
): string => {
  if (!stepKeys.length) return currentKey;
  if (stepKeys.includes(currentKey)) return currentKey;
  const clamped = Math.max(0, Math.min(stepKeys.length - 1, fallbackIndex));
  return stepKeys[clamped];
};

export const stepKeyOffset = (
  stepKeys: string[],
  currentKey: string,
  delta: number,
): string => {
  if (!stepKeys.length) return currentKey;
  const idx = stepKeys.indexOf(currentKey);
  const base = idx >= 0 ? idx : 0;
  const next = Math.max(0, Math.min(stepKeys.length - 1, base + delta));
  return stepKeys[next];
};
