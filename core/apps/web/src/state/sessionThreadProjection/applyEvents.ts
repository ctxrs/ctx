import type { SessionEvent } from "../../api/client";

export function deriveSessionThreadEventsStamp(
  events: SessionEvent[],
  eventsRev?: number,
): string {
  const lastSeq = events.length > 0 ? events[events.length - 1]?.seq ?? 0 : 0;
  return `${eventsRev ?? 0}:${events.length}:${lastSeq}`;
}
