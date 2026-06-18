import type { PluginExtensionRegistry } from "@ctx/types";
import { executePluginCommand } from "../../api/clientSystem";
import { projectPluginCommands } from "./pluginCommandProjection";

export type ResolvePluginCommandMessageOptions = {
  text: string;
  registry: PluginExtensionRegistry;
  workspaceId?: string | null;
  taskId?: string | null;
  sessionId?: string | null;
};

export const resolvePluginCommandMessage = async ({
  text,
  registry,
  workspaceId,
  taskId,
  sessionId,
}: ResolvePluginCommandMessageOptions): Promise<string> => {
  const parsed = parsePluginSlashCommand(text, registry);
  if (!parsed) return text;

  const response = await executePluginCommand({
    plugin_id: parsed.pluginId,
    command_id: parsed.commandId,
    input: parsed.input || null,
    workspace_id: workspaceId || null,
    task_id: taskId || null,
    session_id: sessionId || null,
  });
  if (response.status !== "completed") {
    throw new Error(response.error || `Plugin command ${parsed.commandName} failed`);
  }
  const message = (response.message ?? response.stdout ?? "").trim();
  if (!message) {
    throw new Error(`Plugin command ${parsed.commandName} did not return a message`);
  }
  return message;
};

type ParsedPluginSlashCommand = {
  commandName: string;
  pluginId: string;
  commandId: string;
  input: string;
};

const parsePluginSlashCommand = (
  text: string,
  registry: PluginExtensionRegistry,
): ParsedPluginSlashCommand | null => {
  const trimmed = text.trim();
  if (!trimmed.startsWith("/")) return null;
  const commandEnd = trimmed.search(/\s/);
  const token = commandEnd === -1 ? trimmed.slice(1) : trimmed.slice(1, commandEnd);
  if (!token) return null;

  const command = projectPluginCommands(registry).find((candidate) => candidate.id === token);
  if (!command) return null;

  return {
    commandName: token,
    pluginId: command.pluginId,
    commandId: command.contributionId,
    input: commandEnd === -1 ? "" : trimmed.slice(commandEnd).trim(),
  };
};
