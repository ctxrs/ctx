export const WORKBENCH_TASK_IDLE_EVENT = "ctx:workbench-task-idle" as const;
export const UPDATER_REFRESH_BROADCAST_STORAGE_KEY = "ctx_update_refresh_token_v1" as const;

export type WorkbenchTaskIdleDetail = {
  allTasksIdle: boolean;
};

export type UpdaterRefreshBroadcastDetail = {
  at: number;
  reason: string;
};

export const writeUpdaterRefreshBroadcast = (reason: string): void => {
  if (typeof window === "undefined") return;
  const payload: UpdaterRefreshBroadcastDetail = {
    at: Date.now(),
    reason: String(reason || "").trim(),
  };
  try {
    window.localStorage.setItem(
      UPDATER_REFRESH_BROADCAST_STORAGE_KEY,
      JSON.stringify(payload),
    );
  } catch {
    // Ignore storage write failures; updater checks still run locally.
  }
};
