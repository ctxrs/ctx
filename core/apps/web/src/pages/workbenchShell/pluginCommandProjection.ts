import type {
  PluginCommandContribution,
  PluginContributionRegistration,
  PluginExtensionRegistry,
} from "@ctx/types";
import type { SlashCommandDescriptor } from "../../state/useComposerAutocomplete";

export type PluginCommandPaletteEntry = {
  id: string;
  contributionId: string;
  title: string;
  description: string | null;
  category: string;
  pluginId: string;
  pluginName: string;
  pluginVersion: string;
  entrypoint: string | null;
};

const normalizeText = (value: string | null | undefined): string => String(value ?? "").trim();

const commandSortKey = (entry: PluginCommandPaletteEntry): string =>
  [
    entry.category.toLowerCase(),
    entry.title.toLowerCase(),
    entry.pluginName.toLowerCase(),
    entry.id.toLowerCase(),
  ].join("\0");

export const projectPluginCommands = (
  registry: PluginExtensionRegistry,
): PluginCommandPaletteEntry[] => {
  const entries: PluginCommandPaletteEntry[] = [];
  for (const registration of registry.commands ?? []) {
    const entry = projectPluginCommand(registration);
    if (entry) entries.push(entry);
  }
  return entries.sort((left, right) => commandSortKey(left).localeCompare(commandSortKey(right)));
};

const projectPluginCommand = (
  registration: PluginContributionRegistration<PluginCommandContribution>,
): PluginCommandPaletteEntry | null => {
  const id = normalizeText(registration.contribution.id);
  const pluginId = normalizeText(registration.plugin_id);
  const title = normalizeText(registration.contribution.title);
  const entrypoint = normalizeText(registration.contribution.entrypoint);
  if (!id || !pluginId || !title || !entrypoint) return null;

  return {
    id: `${pluginId}:${id}`,
    contributionId: id,
    title,
    description: normalizeText(registration.contribution.description) || null,
    category: normalizeText(registration.contribution.category) || "Plugins",
    pluginId,
    pluginName: normalizeText(registration.plugin_name) || registration.plugin_id,
    pluginVersion: normalizeText(registration.plugin_version),
    entrypoint,
  };
};

export const projectPluginSlashCommands = (
  registry: PluginExtensionRegistry,
): SlashCommandDescriptor[] =>
  projectPluginCommands(registry).map((entry) => ({
    name: entry.id,
    description: entry.description
      ? `${entry.title} - ${entry.description}`
      : `${entry.title} - ${entry.pluginName}`,
  }));
