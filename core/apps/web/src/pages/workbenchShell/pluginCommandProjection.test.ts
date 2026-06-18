import { describe, expect, it } from "vitest";
import type { PluginExtensionRegistry } from "@ctx/types";
import { projectPluginCommands, projectPluginSlashCommands } from "./pluginCommandProjection";

describe("pluginCommandProjection", () => {
  it("projects active plugin command registrations into command palette entries", () => {
    const registry: PluginExtensionRegistry = {
      revision: 3,
      commands: [
        {
          plugin_id: "zeta.tools",
          plugin_name: "Zeta Tools",
          plugin_version: "0.2.0",
          plugin_path: "/plugins/zeta/ctx-plugin.json",
          plugin_revision: "rev-z",
          contribution: {
            id: "zeta.review",
            title: "Review Diff",
            description: "Run a plugin review pass.",
            category: "Review",
            entrypoint: "main",
          },
        },
        {
          plugin_id: "alpha.tools",
          plugin_name: "Alpha Tools",
          plugin_version: "0.1.0",
          plugin_path: "/plugins/alpha/ctx-plugin.json",
          plugin_revision: "rev-a",
          contribution: {
            id: "alpha.generate",
            title: "Generate Fixture",
            entrypoint: "main",
          },
        },
      ],
    };

    expect(projectPluginCommands(registry)).toEqual([
      {
        id: "alpha.tools:alpha.generate",
        contributionId: "alpha.generate",
        title: "Generate Fixture",
        description: null,
        category: "Plugins",
        pluginId: "alpha.tools",
        pluginName: "Alpha Tools",
        pluginVersion: "0.1.0",
        entrypoint: "main",
      },
      {
        id: "zeta.tools:zeta.review",
        contributionId: "zeta.review",
        title: "Review Diff",
        description: "Run a plugin review pass.",
        category: "Review",
        pluginId: "zeta.tools",
        pluginName: "Zeta Tools",
        pluginVersion: "0.2.0",
        entrypoint: "main",
      },
    ]);
  });

  it("filters malformed command registrations", () => {
    const registry: PluginExtensionRegistry = {
      revision: 1,
      commands: [
        {
          plugin_id: "broken.tools",
          plugin_name: "Broken Tools",
          plugin_version: "0.1.0",
          plugin_path: "/plugins/broken/ctx-plugin.json",
          contribution: {
            id: "broken.command",
            title: "   ",
          },
        },
        {
          plugin_id: "valid.tools",
          plugin_name: "Valid Tools",
          plugin_version: "0.1.0",
          plugin_path: "/plugins/valid/ctx-plugin.json",
          contribution: {
            id: "valid.command",
            title: "Valid Command",
            entrypoint: "main",
          },
        },
        {
          plugin_id: "metadata.tools",
          plugin_name: "Metadata Tools",
          plugin_version: "0.1.0",
          plugin_path: "/plugins/metadata/ctx-plugin.json",
          contribution: {
            id: "metadata.command",
            title: "Metadata Command",
          },
        },
      ],
    };

    expect(projectPluginCommands(registry).map((entry) => entry.id)).toEqual(["valid.tools:valid.command"]);
  });

  it("namespaces command ids by plugin while preserving plugin-local contribution ids", () => {
    const registry: PluginExtensionRegistry = {
      revision: 1,
      commands: [
        {
          plugin_id: "first.tools",
          plugin_name: "First Tools",
          plugin_version: "0.1.0",
          plugin_path: "/plugins/first/ctx-plugin.json",
          contribution: {
            id: "review",
            title: "Review",
            entrypoint: "main",
          },
        },
        {
          plugin_id: "second.tools",
          plugin_name: "Second Tools",
          plugin_version: "0.1.0",
          plugin_path: "/plugins/second/ctx-plugin.json",
          contribution: {
            id: "review",
            title: "Review",
            entrypoint: "main",
          },
        },
      ],
    };

    expect(projectPluginCommands(registry).map(({ id, contributionId }) => ({ id, contributionId }))).toEqual([
      { id: "first.tools:review", contributionId: "review" },
      { id: "second.tools:review", contributionId: "review" },
    ]);
  });

  it("projects plugin commands into namespaced slash command descriptors", () => {
    const registry: PluginExtensionRegistry = {
      revision: 1,
      commands: [
        {
          plugin_id: "review.tools",
          plugin_name: "Review Tools",
          plugin_version: "0.1.0",
          plugin_path: "/plugins/review/ctx-plugin.json",
          contribution: {
            id: "review",
            title: "Review Diff",
            description: "Run a plugin review pass.",
            entrypoint: "main",
          },
        },
      ],
    };

    expect(projectPluginSlashCommands(registry)).toEqual([
      {
        name: "review.tools:review",
        description: "Review Diff - Run a plugin review pass.",
      },
    ]);
  });
});
