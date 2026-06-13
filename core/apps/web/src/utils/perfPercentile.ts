export function percentile(values: readonly number[], p: number): number | null {
  if (values.length === 0) return null;
  const sorted = values.slice().sort((left, right) => left - right);
  const index = Math.min(
    sorted.length - 1,
    Math.max(0, Math.ceil(sorted.length * p) - 1),
  );
  return Math.round(sorted[index]! * 10) / 10;
}

export function percentileSelectsMaximum(sampleCount: number, p: number): boolean {
  if (!Number.isInteger(sampleCount) || sampleCount <= 0) return false;
  if (!Number.isFinite(p) || p <= 0 || p >= 1) return true;
  return Math.ceil(sampleCount * p) - 1 >= sampleCount - 1;
}

export function minSamplesForDistinctPercentile(p: number): number {
  if (!Number.isFinite(p) || p <= 0 || p >= 1) return 1;
  let sampleCount = 1;
  while (percentileSelectsMaximum(sampleCount, p)) {
    sampleCount += 1;
  }
  return sampleCount;
}
