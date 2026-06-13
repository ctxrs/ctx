import React, { createContext, useContext, useEffect, useMemo, useRef, useSyncExternalStore } from "react";
import type {
  WorktreeVcsSnapshot,
  WorktreeVcsStreamClientMessage,
  WorktreeVcsStreamMessage,
  WorktreeVcsStreamTier,
} from "@ctx/types";
import {
  getDaemonClientConfig,
  idToString,
  recordClientCounterMetric,
  recordClientHistogramMetric,
  subscribeDaemonConfig,
} from "../api/client";
import { setBrowserStreamQueryToken } from "../api/browserStreamAuth";
import { parseWsJson } from "../utils/wsJson";

export type WorkspaceVcsState = {
  workspaceId: string;
  connection: "idle" | "connecting" | "connected" | "disconnected";
  demandGeneration: number;
  snapshotsByWorktreeId: Record<string, WorktreeVcsSnapshot>;
};

type WorkspaceVcsDemand = {
  summaryWorktreeIds: string[];
  detailWorktreeIds: string[];
};

type WorkspaceVcsSnapshotMessage = Extract<
  WorktreeVcsStreamMessage,
  { type: "summary_snapshot" | "details_snapshot" | "unavailable_snapshot" }
>;

type QueuedVcsSnapshotMessage = {
  message: WorkspaceVcsSnapshotMessage;
  receivedAtMs: number;
  sequence: number;
};

const VCS_SNAPSHOT_DRAIN_MS = 16;

const emptyDemand = (): WorkspaceVcsDemand => ({
  summaryWorktreeIds: [],
  detailWorktreeIds: [],
});

const normalizeIds = (ids: readonly string[]): string[] =>
  Array.from(
    new Set(
      ids
        .map((value) => idToString(value))
        .filter((value) => value.length > 0),
    ),
  ).sort();

const sameIds = (left: readonly string[], right: readonly string[]): boolean =>
  left.length === right.length && left.every((value, index) => value === right[index]);

const sameDemand = (left: WorkspaceVcsDemand, right: WorkspaceVcsDemand): boolean =>
  sameIds(left.summaryWorktreeIds, right.summaryWorktreeIds) &&
  sameIds(left.detailWorktreeIds, right.detailWorktreeIds);

const snapshotQueueKey = (message: WorkspaceVcsSnapshotMessage): string =>
  `${message.type}:${idToString(message.worktree_id)}`;

const shouldReplaceQueuedSnapshot = (
  previous: WorkspaceVcsSnapshotMessage,
  next: WorkspaceVcsSnapshotMessage,
): boolean => {
  if (next.demand_generation !== previous.demand_generation) {
    return next.demand_generation > previous.demand_generation;
  }
  return next.snapshot.rev >= previous.snapshot.rev;
};

const hasTouchedFileInventory = (snapshot: WorktreeVcsSnapshot): boolean =>
  snapshot.touched_files_state !== "not_loaded" || (snapshot.touched_files.items?.length ?? 0) > 0;

const staleTouchedFilesState = (
  state: WorktreeVcsSnapshot["touched_files_state"],
): WorktreeVcsSnapshot["touched_files_state"] => (state === "ready" ? "stale" : state);

const nowMs = (): number => {
  if (typeof performance !== "undefined" && typeof performance.now === "function") {
    return (performance.timeOrigin ?? Date.now()) + performance.now();
  }
  return Date.now();
};

export class WorkspaceVcsStore {
  private listeners = new Set<() => void>();
  private ws: WebSocket | null = null;
  private reconnectTimer: ReturnType<typeof globalThis.setTimeout> | null = null;
  private configUnsubscribe: (() => void) | null = null;
  private authTokenOverride: string | null = null;
  private wsBaseUrlOverride: string | null = null;
  private destroyed = false;
  private connecting = false;
  private demand = emptyDemand();
  private demandAckPending = false;
  private pendingSnapshotMessages = new Map<string, QueuedVcsSnapshotMessage>();
  private snapshotDrainTimer: ReturnType<typeof globalThis.setTimeout> | null = null;
  private snapshotMessageSequence = 0;
  private snapshot: WorkspaceVcsState;

  constructor(private readonly workspaceId: string) {
    this.snapshot = {
      workspaceId,
      connection: "idle",
      demandGeneration: 0,
      snapshotsByWorktreeId: {},
    };
  }

  init = (): void => {
    this.destroyed = false;
    if (!this.configUnsubscribe && typeof window !== "undefined") {
      this.configUnsubscribe = subscribeDaemonConfig((config) => {
        this.authTokenOverride = config.authToken ?? null;
        this.wsBaseUrlOverride = config.wsBaseUrl ?? null;
        this.reconnectNow();
      });
    }
    this.connect().catch(() => this.scheduleReconnect());
  };

  destroy = (): void => {
    this.destroyed = true;
    this.configUnsubscribe?.();
    this.configUnsubscribe = null;
    if (this.reconnectTimer) {
      globalThis.clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.clearPendingSnapshotMessages();
    try {
      this.ws?.close();
    } catch {
      // ignore
    }
    this.ws = null;
  };

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getSnapshot = (): WorkspaceVcsState => this.snapshot;

  getWorktreeVcsSnapshot = (worktreeId: string): WorktreeVcsSnapshot | null => {
    const id = idToString(worktreeId);
    if (!id) return null;
    return this.snapshot.snapshotsByWorktreeId[id] ?? null;
  };

  setDemand = (demand: {
    summaryWorktreeIds?: readonly string[];
    detailWorktreeIds?: readonly string[];
  }): void => {
    const next = {
      summaryWorktreeIds: normalizeIds(demand.summaryWorktreeIds ?? []),
      detailWorktreeIds: normalizeIds(demand.detailWorktreeIds ?? []),
    };
    if (sameDemand(this.demand, next)) return;
    this.demand = next;
    this.clearPendingSnapshotMessages();
    this.sendDemand();
  };

  refresh = (worktreeIds: readonly string[], tier: WorktreeVcsStreamTier): void => {
    const ids = normalizeIds(worktreeIds);
    if (ids.length === 0) return;
    this.send({
      type: "refresh",
      worktree_ids: ids,
      tier,
    });
  };

  ensureDetailsDemand = (worktreeIds: readonly string[]): void => {
    const ids = normalizeIds(worktreeIds);
    if (ids.length === 0) return;
    this.setDemand({
      summaryWorktreeIds: [...this.demand.summaryWorktreeIds, ...ids],
      detailWorktreeIds: [...this.demand.detailWorktreeIds, ...ids],
    });
  };

  private publish(): void {
    for (const listener of this.listeners) {
      listener();
    }
  }

  private setConnection(connection: WorkspaceVcsState["connection"]): void {
    if (this.snapshot.connection === connection) return;
    this.snapshot = { ...this.snapshot, connection };
    this.publish();
  }

  private async connect(): Promise<void> {
    if (this.destroyed || this.connecting || this.ws) return;
    const config = getDaemonClientConfig();
    const wsBaseUrl = this.wsBaseUrlOverride ?? config.wsBaseUrl;
    const authToken = this.authTokenOverride ?? config.authToken;
    if (!wsBaseUrl) {
      this.scheduleReconnect();
      return;
    }
    this.connecting = true;
    this.setConnection("connecting");
    try {
      const base = wsBaseUrl.replace(/\/+$/, "");
      const query = new URLSearchParams();
      await setBrowserStreamQueryToken(query, authToken, {
        kind: "workspace_vcs",
        workspaceId: this.workspaceId,
      });
      const suffix = query.toString();
      const url = `${base}/api/workspaces/${this.workspaceId}/vcs/stream${suffix ? `?${suffix}` : ""}`;
      const ws = new WebSocket(url);
      this.ws = ws;
      ws.onopen = () => {
        this.setConnection("connected");
        this.sendDemand();
      };
      ws.onmessage = (event: MessageEvent) => {
        this.handleMessage(event.data, nowMs()).catch(() => {});
      };
      ws.onclose = () => {
        if (this.ws === ws) {
          this.ws = null;
        }
        this.setConnection("disconnected");
        this.scheduleReconnect();
      };
      ws.onerror = () => {
        try {
          ws.close();
        } catch {
          // ignore
        }
      };
    } finally {
      this.connecting = false;
    }
  }

  private reconnectNow(): void {
    if (this.destroyed) return;
    if (this.reconnectTimer) {
      globalThis.clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.clearPendingSnapshotMessages();
    const ws = this.ws;
    this.ws = null;
    try {
      ws?.close();
    } catch {
      // ignore
    }
    this.connect().catch(() => this.scheduleReconnect());
  }

  private scheduleReconnect(): void {
    if (this.destroyed || this.reconnectTimer) return;
    this.reconnectTimer = globalThis.setTimeout(() => {
      this.reconnectTimer = null;
      this.connect().catch(() => this.scheduleReconnect());
    }, 1000);
  }

  private sendDemand(): void {
    const ws = this.ws;
    this.demandAckPending = true;
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    const message: WorktreeVcsStreamClientMessage = {
      type: "replace_subscription",
      summary_worktree_ids: this.demand.summaryWorktreeIds,
      detail_worktree_ids: this.demand.detailWorktreeIds,
    };
    try {
      ws.send(JSON.stringify(message));
    } catch {
      // ignore send errors; reconnect owns stream recovery.
    }
  }

  private send(message: WorktreeVcsStreamClientMessage): void {
    const ws = this.ws;
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    try {
      ws.send(JSON.stringify(message));
    } catch {
      // ignore send errors; reconnect owns stream recovery.
    }
  }

  private async handleMessage(data: unknown, receivedAtMs: number): Promise<void> {
    const parsed = await parseWsJson(data);
    if (!parsed || typeof parsed !== "object") return;
    const message = parsed as WorktreeVcsStreamMessage;
    switch (message.type) {
      case "ready":
        break;
      case "subscribed":
        this.demandAckPending = !sameDemand(this.demand, {
          summaryWorktreeIds: normalizeIds(message.summary_worktree_ids),
          detailWorktreeIds: normalizeIds(message.detail_worktree_ids),
        });
        if (message.demand_generation !== this.snapshot.demandGeneration) {
          this.snapshot = {
            ...this.snapshot,
            demandGeneration: message.demand_generation,
          };
          this.publish();
        }
        break;
      case "summary_snapshot":
      case "details_snapshot":
      case "unavailable_snapshot":
        this.queueSnapshotMessage(message, receivedAtMs);
        break;
      case "reset_required":
        this.reconnectNow();
        break;
    }
  }

  private snapshotMatchesCurrentDemand(message: WorkspaceVcsSnapshotMessage): boolean {
    if (this.demandAckPending) return false;
    if (message.demand_generation < this.snapshot.demandGeneration) return false;
    const worktreeId = idToString(message.worktree_id);
    if (!worktreeId) return false;
    const summaryDemanded = this.demand.summaryWorktreeIds.includes(worktreeId);
    const detailsDemanded = this.demand.detailWorktreeIds.includes(worktreeId);
    if (message.type === "details_snapshot") return detailsDemanded;
    if (message.type === "summary_snapshot") return summaryDemanded;
    return summaryDemanded || detailsDemanded;
  }

  private mergeSnapshot(
    previous: WorktreeVcsSnapshot | undefined,
    message: WorkspaceVcsSnapshotMessage,
  ): WorktreeVcsSnapshot | null {
    const incoming = message.snapshot;
    if (!previous) return incoming;
    if (message.type === "summary_snapshot") {
      if (incoming.rev < previous.rev) return null;
      if (hasTouchedFileInventory(previous) && !hasTouchedFileInventory(incoming)) {
        return {
          ...incoming,
          touched_files: previous.touched_files,
          touched_files_state: staleTouchedFilesState(previous.touched_files_state),
        };
      }
      return incoming;
    }
    if (message.type === "details_snapshot") {
      if (incoming.rev >= previous.rev) return incoming;
      if (!hasTouchedFileInventory(incoming)) return null;
      return {
        ...previous,
        touched_files: incoming.touched_files,
        touched_files_state: staleTouchedFilesState(incoming.touched_files_state),
      };
    }
    if (previous.rev >= incoming.rev) return null;
    return incoming;
  }

  private applySnapshotMessage(message: WorkspaceVcsSnapshotMessage, receivedAtMs: number): void {
    if (!this.snapshotMatchesCurrentDemand(message)) return;
    const snapshot = message.snapshot;
    const worktreeId = idToString(snapshot.worktree_id);
    if (!worktreeId) return;
    const previous = this.snapshot.snapshotsByWorktreeId[worktreeId];
    const nextSnapshot = this.mergeSnapshot(previous, message);
    if (!nextSnapshot) return;
    const nextSnapshots = {
      ...this.snapshot.snapshotsByWorktreeId,
      [worktreeId]: nextSnapshot,
    };
    this.snapshot = {
      ...this.snapshot,
      snapshotsByWorktreeId: nextSnapshots,
    };
    recordClientCounterMetric("workspace.vcs_stream.snapshot_count", {
      message_type: message.type,
    });
    if (typeof nextSnapshot.emitted_at_ms === "number" && Number.isFinite(nextSnapshot.emitted_at_ms)) {
      recordClientHistogramMetric(
        "workspace.vcs_stream.receive_lag_ms",
        "ms",
        Math.max(0, receivedAtMs - nextSnapshot.emitted_at_ms),
        { message_type: message.type },
      );
    }
    this.publish();
  }

  private queueSnapshotMessage(message: WorkspaceVcsSnapshotMessage, receivedAtMs: number): void {
    if (!this.snapshotMatchesCurrentDemand(message)) return;
    const key = snapshotQueueKey(message);
    const previous = this.pendingSnapshotMessages.get(key);
    if (!previous || shouldReplaceQueuedSnapshot(previous.message, message)) {
      this.pendingSnapshotMessages.set(key, {
        message,
        receivedAtMs,
        sequence: ++this.snapshotMessageSequence,
      });
    }
    if (this.snapshotDrainTimer) return;
    this.snapshotDrainTimer = globalThis.setTimeout(() => {
      this.snapshotDrainTimer = null;
      this.drainSnapshotMessages();
    }, VCS_SNAPSHOT_DRAIN_MS);
  }

  private drainSnapshotMessages(): void {
    if (this.pendingSnapshotMessages.size === 0) return;
    const messages = Array.from(this.pendingSnapshotMessages.values()).sort(
      (left, right) => left.sequence - right.sequence,
    );
    this.pendingSnapshotMessages.clear();
    for (const queued of messages) {
      this.applySnapshotMessage(queued.message, queued.receivedAtMs);
    }
  }

  private clearPendingSnapshotMessages(): void {
    this.pendingSnapshotMessages.clear();
    if (this.snapshotDrainTimer) {
      globalThis.clearTimeout(this.snapshotDrainTimer);
      this.snapshotDrainTimer = null;
    }
  }
}

const WorkspaceVcsStoreContext = createContext<WorkspaceVcsStore | null>(null);

export function WorkspaceVcsProvider({
  workspaceId,
  children,
}: {
  workspaceId: string;
  children: React.ReactNode;
}) {
  const storeRef = useRef<WorkspaceVcsStore | null>(null);
  const lastWorkspaceRef = useRef<string | null>(null);
  if (!storeRef.current || lastWorkspaceRef.current !== workspaceId) {
    storeRef.current?.destroy();
    storeRef.current = new WorkspaceVcsStore(workspaceId);
    lastWorkspaceRef.current = workspaceId;
  }

  useEffect(() => {
    storeRef.current?.init();
    return () => storeRef.current?.destroy();
  }, [workspaceId]);

  return (
    <WorkspaceVcsStoreContext.Provider value={storeRef.current}>
      {children}
    </WorkspaceVcsStoreContext.Provider>
  );
}

export function useWorkspaceVcsStore(): WorkspaceVcsStore {
  const store = useContext(WorkspaceVcsStoreContext);
  if (!store) throw new Error("WorkspaceVcsProvider missing");
  return store;
}

export function useWorkspaceVcsSnapshot(): WorkspaceVcsState {
  const store = useWorkspaceVcsStore();
  return useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);
}

export function useWorktreeVcsSnapshot(worktreeId: string | null | undefined): WorktreeVcsSnapshot | null {
  const snapshot = useWorkspaceVcsSnapshot();
  return useMemo(() => {
    const id = idToString(worktreeId);
    if (!id) return null;
    return snapshot.snapshotsByWorktreeId[id] ?? null;
  }, [snapshot.snapshotsByWorktreeId, worktreeId]);
}
