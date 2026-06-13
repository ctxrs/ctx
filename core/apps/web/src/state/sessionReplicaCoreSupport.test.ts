import { describe, expect, it } from "vitest";
import type { SessionTurn } from "@ctx/types";
import { mergeReplicaTurns } from "./sessionReplicaCoreSupport";

const mkTurn = (
  status: SessionTurn["status"],
  counts: Partial<
    Pick<
      SessionTurn,
      "tool_total" | "tool_pending" | "tool_running" | "tool_completed" | "tool_failed"
    >
  > = {},
): SessionTurn => ({
  turn_id: "turn-1",
  session_id: "session-1",
  run_id: "run-1",
  user_message_id: "message-1",
  status,
  start_seq: 1,
  end_seq: status === "running" ? null : 4,
  started_at: "2026-03-09T00:00:00.000Z",
  updated_at:
    status === "running" ? "2026-03-09T00:00:00.000Z" : "2026-03-09T00:00:02.000Z",
  assistant_partial: null,
  thought_partial: "",
  metrics_json: null,
  tool_total: counts.tool_total ?? 0,
  tool_pending: counts.tool_pending ?? 0,
  tool_running: counts.tool_running ?? 0,
  tool_completed: counts.tool_completed ?? 0,
  tool_failed: counts.tool_failed ?? 0,
});

describe("sessionReplicaCoreSupport", () => {
  it("lets terminal replica turns clear stale running tool counts", () => {
    const merged = mergeReplicaTurns(
      [mkTurn("running", { tool_total: 1, tool_running: 1 })],
      [mkTurn("failed", { tool_total: 1, tool_failed: 1 })],
    );

    expect(merged[0]?.status).toBe("failed");
    expect(merged[0]?.tool_running).toBe(0);
    expect(merged[0]?.tool_failed).toBe(1);
  });

  it("does not reintroduce running counts from a non-terminal turn into a terminal turn", () => {
    const merged = mergeReplicaTurns(
      [mkTurn("failed", { tool_total: 1, tool_failed: 1 })],
      [mkTurn("running", { tool_total: 1, tool_running: 1 })],
    );

    expect(merged[0]?.status).toBe("failed");
    expect(merged[0]?.tool_running).toBe(0);
    expect(merged[0]?.tool_failed).toBe(1);
  });

  it("normalizes stale live tool counts on terminal replica turns", () => {
    const merged = mergeReplicaTurns([], [mkTurn("failed", { tool_total: 1, tool_running: 1 })]);

    expect(merged[0]?.status).toBe("failed");
    expect(merged[0]?.tool_running).toBe(0);
  });
});
