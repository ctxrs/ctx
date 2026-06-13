import type { SessionActivityState } from "@ctx/types";
import type { SessionTurn } from "../api/client";

const isActiveTurnStatus = (status: SessionActivityState["last_turn_status"] | null | undefined): boolean =>
  status === "running" || status === "starting";

const isTerminalTurnStatus = (status: SessionTurn["status"] | null | undefined): boolean =>
  status === "completed" || status === "failed" || status === "interrupted";

export function isSessionWorkingActivity(
  activity: SessionActivityState | null | undefined,
): boolean {
  return activity?.is_working === true;
}

export function hasSessionActiveTurn(
  activity: SessionActivityState | null | undefined,
  latestTurnStatus?: SessionTurn["status"] | null,
): boolean {
  if (!isActiveTurnStatus(activity?.last_turn_status ?? null)) return false;
  if (isTerminalTurnStatus(latestTurnStatus)) return false;
  return true;
}
