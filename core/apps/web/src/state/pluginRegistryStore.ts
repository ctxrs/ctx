import type { PluginExtensionRegistry, PluginInventoryItem } from "@ctx/types";
import { useEffect, useSyncExternalStore } from "react";
import {
  getPluginExtensions,
  getPlugins,
  reloadPlugins,
  type PluginInventoryResponse,
} from "../api/clientSystem";
import { createDaemonResourceStore } from "./daemonResourceStore";

export type PluginRegistryState = {
  inventoryRevision: number;
  extensionRevision: number;
  roots: string[];
  plugins: PluginInventoryItem[];
  registry: PluginExtensionRegistry;
};

export const EMPTY_PLUGIN_REGISTRY_STATE: PluginRegistryState = Object.freeze({
  inventoryRevision: 0,
  extensionRevision: 0,
  roots: [],
  plugins: [],
  registry: {
    revision: 0,
  },
});

const PLUGIN_REGISTRY_KEY = "global";
const PLUGIN_REGISTRY_REFRESH_INTERVAL_MS = 2_000;

const pluginRegistryStore = createDaemonResourceStore<string, PluginRegistryState>({
  defaultData: EMPTY_PLUGIN_REGISTRY_STATE,
  keyToString: (key) => key,
});

let pluginRegistryWriteGeneration = 0;

const normalizeInventory = (inventory: PluginInventoryResponse): PluginInventoryResponse => ({
  revision: inventory.revision,
  roots: [...(inventory.roots ?? [])].sort((left, right) => left.localeCompare(right)),
  plugins: [...(inventory.plugins ?? [])].sort((left, right) =>
    left.id.localeCompare(right.id) || left.path.localeCompare(right.path)
  ),
});

const normalizeRegistry = (registry: PluginExtensionRegistry): PluginExtensionRegistry => ({
  revision: registry.revision,
  providers: [...(registry.providers ?? [])].sort((left, right) =>
    left.contribution.id.localeCompare(right.contribution.id) || left.plugin_id.localeCompare(right.plugin_id)
  ),
  runtimes: [...(registry.runtimes ?? [])].sort((left, right) =>
    left.contribution.id.localeCompare(right.contribution.id) || left.plugin_id.localeCompare(right.plugin_id)
  ),
  commands: [...(registry.commands ?? [])].sort((left, right) =>
    left.contribution.id.localeCompare(right.contribution.id) || left.plugin_id.localeCompare(right.plugin_id)
  ),
  collectors: [...(registry.collectors ?? [])].sort((left, right) =>
    left.contribution.id.localeCompare(right.contribution.id) || left.plugin_id.localeCompare(right.plugin_id)
  ),
  observers: [...(registry.observers ?? [])].sort((left, right) =>
    left.contribution.id.localeCompare(right.contribution.id) || left.plugin_id.localeCompare(right.plugin_id)
  ),
  ui_surfaces: [...(registry.ui_surfaces ?? [])].sort((left, right) =>
    left.contribution.id.localeCompare(right.contribution.id) || left.plugin_id.localeCompare(right.plugin_id)
  ),
});

const loadPluginRegistryState = async (): Promise<PluginRegistryState> => {
  const [rawInventory, extensionResponse] = await Promise.all([
    getPlugins(),
    getPluginExtensions(),
  ]);
  const inventory = normalizeInventory(rawInventory);
  const registry = normalizeRegistry(extensionResponse.registry);
  return {
    inventoryRevision: inventory.revision,
    extensionRevision: registry.revision,
    roots: inventory.roots,
    plugins: inventory.plugins,
    registry,
  };
};

const loadPluginRegistryStateForGeneration = async (generation: number): Promise<PluginRegistryState> => {
  const next = await loadPluginRegistryState();
  if (generation !== pluginRegistryWriteGeneration) {
    return pluginRegistryStore.getSnapshot(PLUGIN_REGISTRY_KEY);
  }
  return next;
};

export const getCachedPluginRegistryState = (): PluginRegistryState | undefined =>
  pluginRegistryStore.getCached(PLUGIN_REGISTRY_KEY);

export const getPluginRegistrySnapshot = (): PluginRegistryState =>
  pluginRegistryStore.getSnapshot(PLUGIN_REGISTRY_KEY);

export const subscribePluginRegistry = (listener: () => void): (() => void) =>
  pluginRegistryStore.subscribe(PLUGIN_REGISTRY_KEY, listener);

export const loadPluginRegistry = (): Promise<PluginRegistryState> => {
  const generation = pluginRegistryWriteGeneration;
  return pluginRegistryStore.load(
    PLUGIN_REGISTRY_KEY,
    () => loadPluginRegistryStateForGeneration(generation),
  );
};

export const refreshPluginRegistry = (): Promise<PluginRegistryState> => {
  const generation = pluginRegistryWriteGeneration;
  return pluginRegistryStore.refresh(
    PLUGIN_REGISTRY_KEY,
    () => loadPluginRegistryStateForGeneration(generation),
  );
};

export const invalidatePluginRegistry = (): void => {
  pluginRegistryStore.invalidate(PLUGIN_REGISTRY_KEY);
};

export const reloadPluginRegistry = async (): Promise<PluginRegistryState> => {
  const generation = ++pluginRegistryWriteGeneration;
  const inventory = normalizeInventory(await reloadPlugins());
  const extensionResponse = await getPluginExtensions();
  if (generation !== pluginRegistryWriteGeneration) {
    return pluginRegistryStore.getSnapshot(PLUGIN_REGISTRY_KEY);
  }
  const registry = normalizeRegistry(extensionResponse.registry);
  return pluginRegistryStore.update(PLUGIN_REGISTRY_KEY, () => ({
    inventoryRevision: inventory.revision,
    extensionRevision: registry.revision,
    roots: inventory.roots,
    plugins: inventory.plugins,
    registry,
  }));
};

export const usePluginRegistry = (): PluginRegistryState => {
  useEffect(() => {
    void loadPluginRegistry().catch(() => {});
    const interval = window.setInterval(() => {
      void refreshPluginRegistry().catch(() => {});
    }, PLUGIN_REGISTRY_REFRESH_INTERVAL_MS);
    return () => window.clearInterval(interval);
  }, []);
  return useSyncExternalStore(
    subscribePluginRegistry,
    getPluginRegistrySnapshot,
    getPluginRegistrySnapshot,
  );
};
