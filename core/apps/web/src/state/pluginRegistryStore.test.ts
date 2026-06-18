import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  getPluginExtensions,
  getPlugins,
  reloadPlugins,
} from "../api/clientSystem";

const clientSystemMocks = vi.hoisted(() => ({
  getPluginExtensions: vi.fn(),
  getPlugins: vi.fn(),
  reloadPlugins: vi.fn(),
}));

vi.mock("../api/clientSystem", () => clientSystemMocks);

const loadStore = async () => {
  vi.resetModules();
  return import("./pluginRegistryStore");
};

const deferred = <T>() => {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
};

describe("pluginRegistryStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("loads inventory and extension registry into one daemon-wide snapshot", async () => {
    vi.mocked(getPlugins).mockResolvedValue({
      revision: 2,
      roots: ["/plugins/b", "/plugins/a"],
      plugins: [
        {
          id: "zeta.tools",
          name: "Zeta Tools",
          version: "0.2.0",
          enabled: "enabled",
          status: "loaded",
          path: "/plugins/zeta/ctx-plugin.json",
        },
        {
          id: "alpha.tools",
          name: "Alpha Tools",
          version: "0.1.0",
          enabled: "enabled",
          status: "loaded",
          path: "/plugins/alpha/ctx-plugin.json",
        },
      ],
    });
    vi.mocked(getPluginExtensions).mockResolvedValue({
      registry: {
        revision: 2,
        commands: [
          {
            plugin_id: "zeta.tools",
            plugin_name: "Zeta Tools",
            plugin_version: "0.2.0",
            plugin_path: "/plugins/zeta/ctx-plugin.json",
            contribution: {
              id: "zeta.review",
              title: "Review Diff",
            },
          },
        ],
      },
    });
    const store = await loadStore();

    const loaded = await store.loadPluginRegistry();

    expect(loaded.inventoryRevision).toBe(2);
    expect(loaded.extensionRevision).toBe(2);
    expect(loaded.roots).toEqual(["/plugins/a", "/plugins/b"]);
    expect(loaded.plugins.map((plugin) => plugin.id)).toEqual(["alpha.tools", "zeta.tools"]);
    expect(loaded.registry.commands?.[0]?.contribution.id).toBe("zeta.review");
    expect(store.getPluginRegistrySnapshot()).toEqual(loaded);
  });

  it("reloads plugins then refreshes the active extension registry", async () => {
    vi.mocked(reloadPlugins).mockResolvedValue({
      revision: 3,
      roots: ["/plugins"],
      plugins: [
        {
          id: "alpha.tools",
          name: "Alpha Tools",
          version: "0.1.0",
          enabled: "enabled",
          status: "loaded",
          path: "/plugins/alpha/ctx-plugin.json",
        },
      ],
    });
    vi.mocked(getPluginExtensions).mockResolvedValue({
      registry: {
        revision: 3,
        providers: [
          {
            plugin_id: "alpha.tools",
            plugin_name: "Alpha Tools",
            plugin_version: "0.1.0",
            plugin_path: "/plugins/alpha/ctx-plugin.json",
            contribution: {
              id: "alpha-provider",
              name: "Alpha Provider",
            },
          },
        ],
      },
    });
    const store = await loadStore();

    const reloaded = await store.reloadPluginRegistry();

    expect(reloadPlugins).toHaveBeenCalledTimes(1);
    expect(getPluginExtensions).toHaveBeenCalledTimes(1);
    expect(reloaded.inventoryRevision).toBe(3);
    expect(reloaded.extensionRevision).toBe(3);
    expect(reloaded.registry.providers?.[0]?.contribution.id).toBe("alpha-provider");
  });

  it("does not let an older in-flight load overwrite a newer reload", async () => {
    const loadInventory = deferred<Awaited<ReturnType<typeof getPlugins>>>();
    const loadExtensions = deferred<Awaited<ReturnType<typeof getPluginExtensions>>>();

    vi.mocked(getPlugins).mockReturnValueOnce(loadInventory.promise);
    vi.mocked(getPluginExtensions)
      .mockReturnValueOnce(loadExtensions.promise)
      .mockResolvedValueOnce({
        registry: {
          revision: 5,
          commands: [
            {
              plugin_id: "fresh.tools",
              plugin_name: "Fresh Tools",
              plugin_version: "0.2.0",
              plugin_path: "/plugins/fresh/ctx-plugin.json",
              contribution: {
                id: "fresh.command",
                title: "Fresh Command",
              },
            },
          ],
        },
      });
    vi.mocked(reloadPlugins).mockResolvedValue({
      revision: 5,
      roots: ["/plugins/fresh"],
      plugins: [
        {
          id: "fresh.tools",
          name: "Fresh Tools",
          version: "0.2.0",
          enabled: "enabled",
          status: "loaded",
          path: "/plugins/fresh/ctx-plugin.json",
        },
      ],
    });
    const store = await loadStore();

    const loadPromise = store.loadPluginRegistry();
    const reloadPromise = store.reloadPluginRegistry();
    await reloadPromise;

    loadInventory.resolve({
      revision: 1,
      roots: ["/plugins/stale"],
      plugins: [
        {
          id: "stale.tools",
          name: "Stale Tools",
          version: "0.1.0",
          enabled: "enabled",
          status: "loaded",
          path: "/plugins/stale/ctx-plugin.json",
        },
      ],
    });
    loadExtensions.resolve({
      registry: {
        revision: 1,
        commands: [
          {
            plugin_id: "stale.tools",
            plugin_name: "Stale Tools",
            plugin_version: "0.1.0",
            plugin_path: "/plugins/stale/ctx-plugin.json",
            contribution: {
              id: "stale.command",
              title: "Stale Command",
            },
          },
        ],
      },
    });
    await loadPromise;

    expect(store.getPluginRegistrySnapshot().inventoryRevision).toBe(5);
    expect(store.getPluginRegistrySnapshot().plugins.map((plugin) => plugin.id)).toEqual(["fresh.tools"]);
    expect(store.getPluginRegistrySnapshot().registry.commands?.[0]?.contribution.id).toBe("fresh.command");
  });
});
