import type { SessionHeadSnapshot } from "@ctx/types";

import { idToString } from "../api/client";
import { compactActiveSessionHeadSnapshot } from "./sessionHeadState";
import { shouldReplaceSessionHead } from "./workspaceActiveSnapshot/summaryHelpers";

export type SessionHeadBootstrapRecord = Record<string, SessionHeadSnapshot>;

type PersistedPrefetchEntry = {
  token: symbol;
  promise: Promise<void>;
  resolve: () => void;
};

type AuthoritativePrefetchEntry = {
  sessionId: string;
  versionKey: string;
  force: boolean;
  token: symbol;
  promise: Promise<AuthoritativePrefetchCompletion>;
  resolve: (outcome: AuthoritativePrefetchCompletion) => void;
};

export type PersistedPrefetchLease =
  | { state: "skip" }
  | { state: "wait"; promise: Promise<void> }
  | { state: "start"; finish: (completed: boolean) => void };

export type AuthoritativePrefetchLease =
  | { state: "skip" }
  | { state: "throttled" }
  | { state: "wait"; promise: Promise<AuthoritativePrefetchCompletion> }
  | { state: "start"; finish: (outcome: AuthoritativePrefetchCompletion) => void };

export type AuthoritativePrefetchCompletion =
  | "success"
  | "stale"
  | "missing"
  | "failed"
  | "not_retained"
  | "canceled";

const AUTHORITATIVE_PREFETCH_RETRY_COOLDOWN_MS = 15_000;

export class SessionHeadBootstrapCache {
  private readonly entries = new Map<string, SessionHeadSnapshot>();
  private readonly persistedPrefetchCompletedSessionIds = new Set<string>();
  private readonly persistedPrefetchInFlight = new Map<string, PersistedPrefetchEntry>();
  private readonly authoritativePrefetchInFlight = new Map<string, AuthoritativePrefetchEntry>();
  private readonly authoritativePrefetchVersions = new Map<string, string>();
  private readonly authoritativePrefetchRetryAfterMs = new Map<string, number>();

  upsert(head: SessionHeadSnapshot | null | undefined): boolean {
    const sessionId = idToString(head?.session?.id);
    if (!sessionId) return false;
    if (!head) return false;
    const compacted = compactActiveSessionHeadSnapshot(head);
    const previous = this.entries.get(sessionId);
    if (!shouldReplaceSessionHead(previous, compacted)) return false;
    this.entries.set(sessionId, compacted);
    return true;
  }

  upsertAll(heads: SessionHeadBootstrapRecord): boolean {
    let changed = false;
    for (const head of Object.values(heads)) {
      if (this.upsert(head)) {
        changed = true;
      }
    }
    return changed;
  }

  snapshot(): SessionHeadBootstrapRecord {
    return Object.fromEntries(this.entries) as SessionHeadBootstrapRecord;
  }

  get(sessionId: string | null | undefined): SessionHeadSnapshot | undefined {
    const id = idToString(sessionId ?? "");
    if (!id) return undefined;
    return this.entries.get(id);
  }

  retain(sessionIds: readonly string[]): void {
    const allowed = new Set(
      sessionIds
        .map((sessionId) => idToString(sessionId))
        .filter((sessionId) => sessionId.length > 0),
    );
    for (const sessionId of this.entries.keys()) {
      if (!allowed.has(sessionId)) {
        this.entries.delete(sessionId);
      }
    }
    for (const sessionId of this.persistedPrefetchCompletedSessionIds) {
      if (!allowed.has(sessionId)) {
        this.persistedPrefetchCompletedSessionIds.delete(sessionId);
      }
    }
    for (const sessionId of this.persistedPrefetchInFlight.keys()) {
      if (!allowed.has(sessionId)) {
        this.finishPersistedPrefetch(sessionId, null, false);
      }
    }
    for (const [sessionId, entry] of this.authoritativePrefetchInFlight.entries()) {
      if (!allowed.has(entry.sessionId)) {
        this.authoritativePrefetchInFlight.delete(sessionId);
        entry.resolve("canceled");
      }
    }
    for (const sessionId of this.authoritativePrefetchVersions.keys()) {
      if (!allowed.has(sessionId)) {
        this.authoritativePrefetchVersions.delete(sessionId);
      }
    }
    for (const sessionId of this.authoritativePrefetchRetryAfterMs.keys()) {
      if (!allowed.has(sessionId)) {
        this.authoritativePrefetchRetryAfterMs.delete(sessionId);
      }
    }
  }

  beginPersistedPrefetch(sessionId: string | null | undefined): PersistedPrefetchLease {
    const id = String(sessionId ?? "").trim();
    if (!id) return { state: "skip" };
    if (this.persistedPrefetchCompletedSessionIds.has(id)) {
      return { state: "skip" };
    }
    const inFlight = this.persistedPrefetchInFlight.get(id);
    if (inFlight) {
      return { state: "wait", promise: inFlight.promise };
    }
    const token = Symbol(id);
    let resolve = () => {};
    const promise = new Promise<void>((resolver) => {
      resolve = resolver;
    });
    this.persistedPrefetchInFlight.set(id, { token, promise, resolve });
    return {
      state: "start",
      finish: (completed: boolean) => {
        this.finishPersistedPrefetch(id, token, completed);
      },
    };
  }

  private finishPersistedPrefetch(
    sessionId: string,
    token: symbol | null,
    completed: boolean,
  ): void {
    const entry = this.persistedPrefetchInFlight.get(sessionId);
    if (!entry) {
      if (!completed) {
        this.persistedPrefetchCompletedSessionIds.delete(sessionId);
      }
      return;
    }
    if (token && entry.token !== token) return;
    this.persistedPrefetchInFlight.delete(sessionId);
    if (completed) {
      this.persistedPrefetchCompletedSessionIds.add(sessionId);
    } else {
      this.persistedPrefetchCompletedSessionIds.delete(sessionId);
    }
    entry.resolve();
  }

  beginAuthoritativePrefetch(
    sessionId: string | null | undefined,
    versionKey: string | null | undefined,
    opts?: { force?: boolean; nowMs?: number },
  ): AuthoritativePrefetchLease {
    const id = String(sessionId ?? "").trim();
    const normalizedVersionKey = String(versionKey ?? "").trim();
    if (!id || !normalizedVersionKey) return { state: "skip" };
    if (this.authoritativePrefetchVersions.get(id) === normalizedVersionKey) {
      return { state: "skip" };
    }
    const inFlight = this.authoritativePrefetchInFlight.get(id);
    if (inFlight) {
      if (opts?.force) {
        inFlight.force = true;
      }
      return { state: "wait", promise: inFlight.promise };
    }
    const nowMs = opts?.nowMs ?? Date.now();
    const retryAfterMs = this.authoritativePrefetchRetryAfterMs.get(id);
    if (!opts?.force && typeof retryAfterMs === "number" && retryAfterMs > nowMs) {
      return { state: "throttled" };
    }
    this.authoritativePrefetchRetryAfterMs.delete(id);
    const token = Symbol(id);
    let resolve = (_outcome: AuthoritativePrefetchCompletion) => {};
    const promise = new Promise<AuthoritativePrefetchCompletion>((resolver) => {
      resolve = resolver;
    });
    const entry: AuthoritativePrefetchEntry = {
      sessionId: id,
      versionKey: normalizedVersionKey,
      force: Boolean(opts?.force),
      token,
      promise,
      resolve,
    };
    this.authoritativePrefetchInFlight.set(id, entry);
    return {
      state: "start",
      finish: (outcome: AuthoritativePrefetchCompletion) => {
        this.finishAuthoritativePrefetch(entry, outcome);
      },
    };
  }

  private finishAuthoritativePrefetch(
    entry: AuthoritativePrefetchEntry,
    outcome: AuthoritativePrefetchCompletion,
  ): void {
    const current = this.authoritativePrefetchInFlight.get(entry.sessionId);
    if (current?.token === entry.token) {
      this.authoritativePrefetchInFlight.delete(entry.sessionId);
      if (outcome === "success") {
        this.authoritativePrefetchVersions.set(entry.sessionId, entry.versionKey);
        this.authoritativePrefetchRetryAfterMs.delete(entry.sessionId);
      } else if (!entry.force && (outcome === "missing" || outcome === "failed")) {
        this.authoritativePrefetchRetryAfterMs.set(
          entry.sessionId,
          Date.now() + AUTHORITATIVE_PREFETCH_RETRY_COOLDOWN_MS,
        );
      }
    }
    entry.resolve(outcome);
  }

  clear(): void {
    this.entries.clear();
    for (const sessionId of this.persistedPrefetchInFlight.keys()) {
      this.finishPersistedPrefetch(sessionId, null, false);
    }
    this.persistedPrefetchCompletedSessionIds.clear();
    for (const entry of this.authoritativePrefetchInFlight.values()) {
      entry.resolve("canceled");
    }
    this.authoritativePrefetchInFlight.clear();
    this.authoritativePrefetchVersions.clear();
    this.authoritativePrefetchRetryAfterMs.clear();
  }
}
