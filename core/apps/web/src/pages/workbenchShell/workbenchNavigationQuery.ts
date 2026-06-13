export type WorkbenchNavigationTarget = {
  sessionId: string | null;
  taskId: string;
};

export const readWorkbenchNavigationTarget = (
  search: string,
): WorkbenchNavigationTarget | null => {
  const params = new URLSearchParams(search);
  const taskId = String(params.get("task") ?? "").trim();
  if (!taskId) return null;
  const sessionId = String(params.get("session") ?? "").trim();
  return {
    taskId,
    sessionId: sessionId || null,
  };
};

export const stripWorkbenchNavigationTarget = (search: string): string => {
  const params = new URLSearchParams(search);
  params.delete("task");
  params.delete("session");
  return params.toString();
};
