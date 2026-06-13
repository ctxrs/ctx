import type { ExecutionEnvironment } from "@ctx/types";
import { idToString, type Session } from "../../api/client";
import type { AnalyticsSessionKind } from "../../utils/analytics/types";
import { composeModelId } from "../../utils/modelEffort";

export type TurnAnalyticsMetadata = {
  sessionId: string;
  taskId?: string;
  workspaceId?: string;
  providerId?: string;
  modelId?: string;
  reasoningEffort?: string;
  executionEnvironment?: ExecutionEnvironment;
  sessionKind: AnalyticsSessionKind;
  title?: string;
};

export const resolveTurnAnalyticsMetadata = (
  session: Session | null | undefined,
  fallbackSessionId: string,
): TurnAnalyticsMetadata => {
  const baseModelId = String(session?.model_id ?? "").trim();
  const reasoningEffort = String(session?.reasoning_effort ?? "").trim();
  const modelId =
    composeModelId(baseModelId, reasoningEffort || null) || baseModelId || undefined;
  return {
    sessionId: idToString(session?.id ?? fallbackSessionId),
    taskId: idToString(session?.task_id ?? "") || undefined,
    workspaceId: idToString(session?.workspace_id ?? "") || undefined,
    providerId: String(session?.provider_id ?? "").trim() || undefined,
    modelId,
    reasoningEffort: reasoningEffort || undefined,
    executionEnvironment: session?.execution_environment ?? undefined,
    sessionKind:
      session?.parent_session_id || session?.relationship === "sub_agent"
        ? "subagent"
        : "primary",
    title: session?.title ? String(session.title) : undefined,
  };
};
