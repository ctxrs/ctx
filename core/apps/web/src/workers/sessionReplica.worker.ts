import type { SessionHeadSnapshot, SessionSnapshot, SessionState } from "@ctx/types";
import { workerFetchJson, setWorkerClientConfig } from "../api/workerClient";
import { SessionReplicaCore } from "../state/sessionReplicaCore";
import type { SessionReplicaCommand, SessionReplicaWorkerMessage } from "../state/sessionReplicaProtocol";

const setAuth = (baseUrl?: string | null, authToken?: string | null, runId?: string | null) => {
  setWorkerClientConfig({ baseUrl, authToken, runId });
};

const api = {
  getSessionSnapshot: (sessionId: string, limit?: number, includeEvents?: boolean): Promise<SessionSnapshot> => {
    const qs = new URLSearchParams();
    if (limit) qs.set("limit", String(limit));
    if (includeEvents !== undefined) qs.set("include_events", includeEvents ? "1" : "0");
    const suffix = qs.toString() ? `?${qs.toString()}` : "";
    return workerFetchJson<SessionSnapshot>(`/api/sessions/${sessionId}/snapshot${suffix}`);
  },
  getSessionHead: (
    sessionId: string,
    limit?: number,
    includeEvents?: boolean,
    opts?: { minEventSeq?: number },
  ): Promise<SessionHeadSnapshot> => {
    const qs = new URLSearchParams();
    if (limit) qs.set("limit", String(limit));
    if (includeEvents !== undefined) qs.set("include_events", includeEvents ? "1" : "0");
    if (typeof opts?.minEventSeq === "number" && Number.isFinite(opts.minEventSeq)) {
      qs.set("min_event_seq", String(opts.minEventSeq));
    }
    const suffix = qs.toString() ? `?${qs.toString()}` : "";
    return workerFetchJson<SessionHeadSnapshot>(`/api/sessions/${sessionId}/head${suffix}`);
  },
  getSessionState: (sessionId: string): Promise<SessionState> =>
    workerFetchJson<SessionState>(`/api/sessions/${sessionId}/state`),
  setAuth,
};

const core = new SessionReplicaCore({
  api,
  emit: (patches) => {
    const message: SessionReplicaWorkerMessage = { type: "patches", patches };
    self.postMessage(message);
  },
  emitFreshness: (event) => {
    const message: SessionReplicaWorkerMessage = { type: "freshness_event", event };
    self.postMessage(message);
  },
});

self.onmessage = (event: MessageEvent<SessionReplicaCommand>) => {
  const cmd = event.data;
  if (!cmd) return;
  core.handleCommand(cmd);
};
