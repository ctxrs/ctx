import type { ProviderOptions, SessionEvent } from "../../api/client";
import { pickFirstString } from "./eventNormalization";

const asRecord = (value: unknown): Record<string, unknown> =>
  value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};

export type AcpMeta = {
  models?: unknown;
  modes?: unknown;
  currentModelId?: string;
  commands?: unknown;
  slashCommands?: unknown;
};

export const readAcpCurrentModelId = (models: unknown): string | undefined => {
  const record = asRecord(models);
  if (!record) return;
  const modelId = record.currentModelId ?? record.current_model_id;
  return typeof modelId === "string" ? modelId : undefined;
};

export const hasModelList = (models: unknown): boolean => {
  const record = asRecord(models);
  if (!record) return false;
  const list =
    record.availableModels ??
    record.available_models ??
    record.models ??
    [];
  return Array.isArray(list) && list.length > 0;
};

export const extractAcpMetaFromEvent = (event: SessionEvent): AcpMeta | null => {
  if (event.event_type !== "init") return null;
  const payload = asRecord(event.payload_json);
  if (!payload) return null;
  const models = payload.models ?? undefined;
  const modes = payload.modes ?? undefined;
  const commands = payload.commands ?? undefined;
  const slashCommands = payload.slashCommands ?? payload.slash_commands ?? undefined;
  const currentModelId =
    pickFirstString(payload.currentModelId, payload.current_model_id) ?? readAcpCurrentModelId(models);
  if (!models && !modes && !commands && !slashCommands && !currentModelId) return null;
  return {
    models,
    modes,
    currentModelId,
    commands,
    slashCommands,
  };
};

const mergeSharedProviderModelsFromAcpMeta = (
  previousModels: unknown,
  nextModels: unknown,
  sourceKind: string | undefined,
): unknown => {
  const nextRecord = asRecord(nextModels);
  if (Object.keys(nextRecord).length === 0) return previousModels;

  const previousRecord = asRecord(previousModels);
  const previousCurrentModelId =
    pickFirstString(previousRecord.currentModelId, previousRecord.current_model_id);
  const nextCurrentModelId =
    pickFirstString(nextRecord.currentModelId, nextRecord.current_model_id);
  const previousMeta = asRecord(previousRecord.meta);
  const nextMeta = asRecord(nextRecord.meta);

  const merged: Record<string, unknown> = {
    ...previousRecord,
    ...nextRecord,
    meta: {
      ...previousMeta,
      ...nextMeta,
      source_kind: sourceKind ?? nextMeta.source_kind ?? previousMeta.source_kind ?? "subscription",
      catalog_source: "session_acp_live",
      refresh_pending: false,
    },
  };

  const sharedCurrentModelId = previousCurrentModelId ?? nextCurrentModelId;
  if (sharedCurrentModelId) {
    merged.current_model_id = sharedCurrentModelId;
    delete merged.currentModelId;
  }

  return JSON.stringify(previousModels ?? null) === JSON.stringify(merged) ? previousModels : merged;
};

export const mergeAcpMetaIntoSharedProviderOptions = (
  existing: ProviderOptions | undefined,
  meta: AcpMeta,
  providerId: string,
  workspaceId: string,
): ProviderOptions | undefined => {
  if (!meta.models && !meta.modes) return existing;

  const nextModels = meta.models !== undefined
    ? mergeSharedProviderModelsFromAcpMeta(
      existing?.models,
      meta.models,
      existing?.source?.selected_source_kind,
    )
    : existing?.models;
  const nextModes = meta.modes ?? existing?.modes;
  const hasModelChange = JSON.stringify(existing?.models ?? null) !== JSON.stringify(nextModels ?? null);
  const hasModeChange = JSON.stringify(existing?.modes ?? null) !== JSON.stringify(nextModes ?? null);
  if (!hasModelChange && !hasModeChange) {
    return existing;
  }

  const providerOptions: ProviderOptions = existing ?? {
    provider_id: providerId,
    workspace_id: workspaceId,
    supports_load: false,
    auth_required: false,
    probed_at: new Date().toISOString(),
  };

  return {
    ...providerOptions,
    models: nextModels,
    modes: nextModes,
    probed_at: new Date().toISOString(),
  };
};
