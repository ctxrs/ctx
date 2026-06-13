export type ForegroundFreshnessSurface =
  | "final_delivery"
  | "interrupt"
  | "session_switch"
  | "gap_recovery"
  | "workspace_backlog"
  | "foreground_backlog"
  | "desktop_startup";

export type ForegroundFreshnessQueueLane = "foreground" | "workspace";

export const severityBucketForDuration = (
  durationMs: number,
): "slight" | "moderate" | "severe" => {
  if (durationMs < 250) return "slight";
  if (durationMs < 1000) return "moderate";
  return "severe";
};

export const gapBucketForDuration = (
  durationMs: number,
): "under_250ms" | "250ms_to_1000ms" | "1000ms_plus" => {
  if (durationMs < 250) return "under_250ms";
  if (durationMs < 1000) return "250ms_to_1000ms";
  return "1000ms_plus";
};

export const backlogBucketForDuration = (
  durationMs: number,
): "over_75ms" | "over_250ms" | "over_1000ms" => {
  if (durationMs >= 1000) return "over_1000ms";
  if (durationMs >= 250) return "over_250ms";
  return "over_75ms";
};
