import type { APIRequestContext } from "@playwright/test";

export type AcceptanceSeedSource = {
  providerId: string;
  modelId: string;
  executionEnvironment: string;
};

const DEFAULT_WORKSPACE_ID = process.env.MESSAGE_LIST_WORKSPACE_ID ?? "3d6ade3f-f141-4e64-8156-7f746879decf";

export async function resolveAcceptanceSeedSource(
  request: APIRequestContext,
  workspaceId = DEFAULT_WORKSPACE_ID,
): Promise<AcceptanceSeedSource> {
  const tasksResp = await request.get(`/api/workspaces/${workspaceId}/tasks`);
  if (!tasksResp.ok()) {
    throw new Error(`seed source task list failed (${tasksResp.status()})`);
  }
  const tasks = (await tasksResp.json()) as Array<{ id?: string | null }>;

  for (const task of tasks) {
    const taskId = typeof task.id === "string" ? task.id : "";
    if (!taskId) continue;
    const sessionsResp = await request.get(`/api/tasks/${taskId}/sessions`);
    if (!sessionsResp.ok()) continue;
    const sessions = (await sessionsResp.json()) as Array<{
      provider_id?: string | null;
      model_id?: string | null;
      execution_environment?: string | null;
    }>;
    const session = sessions.find(
      (entry) =>
        typeof entry.provider_id === "string" &&
        entry.provider_id.length > 0 &&
        typeof entry.model_id === "string" &&
        entry.model_id.length > 0,
    );
    if (!session) continue;
    return {
      providerId: session.provider_id ?? "",
      modelId: session.model_id ?? "",
      executionEnvironment: session.execution_environment ?? "host",
    };
  }

  throw new Error(`unable to resolve acceptance seed source from workspace ${workspaceId}`);
}
