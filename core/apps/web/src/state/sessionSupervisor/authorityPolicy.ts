import {
  isAuthoritativeSessionReplicaReplace,
  type SessionReplicaData,
  type SessionReplicaPatch,
} from "../sessionReplicaProtocol";
import { idToString } from "../../api/client";
import { isReplicaAuthority } from "./config";
import type { InternalEntry, SessionLoadState } from "./entryState";

const hasDurableSeq = (value: number | undefined): value is number =>
  typeof value === "number" && value >= 0;

export const hasVisibleBootstrapOnlyTranscript = (
  entry: Pick<InternalEntry, "mode" | "freshness">,
): boolean => entry.mode === "active" && !isReplicaAuthority(entry.freshness);

export const resolveReplicaReadyLoadState = (
  entry: Pick<InternalEntry, "mode" | "freshness">,
): SessionLoadState => (hasVisibleBootstrapOnlyTranscript(entry) ? "pending_hydration" : "live");

export const hasSessionReplicaRecoveryData = (data: SessionReplicaData): boolean =>
  data.session !== undefined ||
  (Array.isArray(data.turns) && data.turns.length > 0) ||
  (Array.isArray(data.messages) && data.messages.length > 0) ||
  (Array.isArray(data.events) && data.events.length > 0) ||
  (Array.isArray(data.toolSummaries) && data.toolSummaries.length > 0) ||
  data.lastEventSeq !== undefined ||
  data.projectionRev !== undefined ||
  data.stateRev !== undefined ||
  data.summaryCheckpoint !== undefined ||
  data.headWindow !== undefined ||
  data.hasMoreTurns !== undefined ||
  data.turnsHydrated !== undefined;

const hasCanonicalTranscriptDifference = (
  entry: Pick<InternalEntry, "activity" | "events" | "messages" | "toolSummaries" | "turns">,
  data: SessionReplicaData,
): boolean => {
  if (Array.isArray(data.turns)) {
    const currentTurns = new Map(entry.turns.map((turn) => [idToString(turn.turn_id), turn]));
    for (const turn of data.turns) {
      const turnId = idToString(turn.turn_id);
      const current = currentTurns.get(turnId);
      if (!current) return true;
      if (
        current.status !== turn.status ||
        current.end_seq !== turn.end_seq ||
        idToString(current.user_message_id ?? "") !== idToString(turn.user_message_id ?? "") ||
        current.tool_pending !== turn.tool_pending ||
        current.tool_running !== turn.tool_running ||
        current.tool_completed !== turn.tool_completed ||
        current.tool_failed !== turn.tool_failed
      ) {
        return true;
      }
    }
  }

  if (Array.isArray(data.messages)) {
    const currentMessages = new Map(entry.messages.map((message) => [idToString(message.id), message]));
    for (const message of data.messages) {
      const messageId = idToString(message.id);
      const current = currentMessages.get(messageId);
      if (!current) return true;
      if (
        current.role !== message.role ||
        current.content !== message.content ||
        current.delivery !== message.delivery ||
        idToString(current.turn_id ?? "") !== idToString(message.turn_id ?? "")
      ) {
        return true;
      }
    }
  }

  if (Array.isArray(data.events)) {
    const currentEvents = new Map(
      entry.events
        .filter((event) => typeof event.seq === "number")
        .map((event) => [event.seq as number, event]),
    );
    for (const event of data.events) {
      if (typeof event.seq !== "number") continue;
      const current = currentEvents.get(event.seq);
      if (!current) return true;
      if (
        current.id !== event.id ||
        current.event_type !== event.event_type ||
        idToString(current.turn_id ?? "") !== idToString(event.turn_id ?? "")
      ) {
        return true;
      }
    }
  }

  if (Array.isArray(data.toolSummaries)) {
    const currentSummaries = new Map(
      entry.toolSummaries.map((summary) => [String(summary.tool_call_id ?? "").trim(), summary]),
    );
    for (const summary of data.toolSummaries) {
      const toolCallId = String(summary.tool_call_id ?? "").trim();
      const current = currentSummaries.get(toolCallId);
      if (!current) return true;
      if (
        current.status !== summary.status ||
        current.output_preview !== summary.output_preview ||
        current.output_truncated !== summary.output_truncated ||
        current.output_original_bytes !== summary.output_original_bytes
      ) {
        return true;
      }
    }
  }

  if (data.activity !== undefined) {
    const current = entry.activity ?? null;
    const incoming = data.activity ?? null;
    if (
      current?.is_working !== incoming?.is_working ||
      current?.last_turn_status !== incoming?.last_turn_status
    ) {
      return true;
    }
  }

  return false;
};

export const shouldReplayReplicaReplace = ({
  entry,
  patch,
  normalizedFreshness,
}: {
  entry: Pick<
    InternalEntry,
    "activity" | "events" | "freshness" | "lastEventSeq" | "messages" | "projectionRev" | "toolSummaries" | "turns"
  >;
  patch: SessionReplicaPatch;
  normalizedFreshness?: InternalEntry["freshness"];
}): boolean => {
  if (patch.op !== "replace") return true;
  if (!isReplicaAuthority(entry.freshness)) return true;
  if (normalizedFreshness === "recovering") return true;
  if (patch.data.replaceMode === "repair_replace" && normalizedFreshness === "replica") {
    return true;
  }
  if (!isAuthoritativeSessionReplicaReplace(patch.data.replaceMode) || normalizedFreshness !== "replica") {
    return false;
  }

  const incomingProjectionRev =
    typeof patch.data.projectionRev === "number" ? patch.data.projectionRev : undefined;
  const currentProjectionRev =
    typeof entry.projectionRev === "number" ? entry.projectionRev : undefined;
  if (hasDurableSeq(incomingProjectionRev) && !hasDurableSeq(currentProjectionRev)) return true;
  if (
    hasDurableSeq(incomingProjectionRev) &&
    hasDurableSeq(currentProjectionRev) &&
    incomingProjectionRev > currentProjectionRev
  ) {
    return true;
  }

  const incomingLastEventSeq =
    typeof patch.data.lastEventSeq === "number" ? patch.data.lastEventSeq : undefined;
  const currentLastEventSeq =
    typeof entry.lastEventSeq === "number" ? entry.lastEventSeq : undefined;
  if (hasDurableSeq(incomingLastEventSeq) && !hasDurableSeq(currentLastEventSeq)) return true;
  if (
    hasDurableSeq(incomingLastEventSeq) &&
    hasDurableSeq(currentLastEventSeq) &&
    incomingLastEventSeq > currentLastEventSeq
  ) {
    return true;
  }

  if (
    hasDurableSeq(incomingLastEventSeq) &&
    hasDurableSeq(currentLastEventSeq) &&
    incomingLastEventSeq < currentLastEventSeq
  ) {
    return false;
  }
  if (
    !hasDurableSeq(incomingLastEventSeq) &&
    !hasDurableSeq(currentLastEventSeq) &&
    hasDurableSeq(incomingProjectionRev) &&
    hasDurableSeq(currentProjectionRev) &&
    incomingProjectionRev < currentProjectionRev
  ) {
    return false;
  }
  const equalVersion =
    (hasDurableSeq(incomingProjectionRev) &&
      hasDurableSeq(currentProjectionRev) &&
      incomingProjectionRev === currentProjectionRev) ||
    (hasDurableSeq(incomingLastEventSeq) &&
      hasDurableSeq(currentLastEventSeq) &&
      incomingLastEventSeq === currentLastEventSeq);
  return equalVersion && hasCanonicalTranscriptDifference(entry, patch.data);
};
