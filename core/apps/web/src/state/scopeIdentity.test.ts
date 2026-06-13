import { describe, expect, it } from "vitest";
import type { ProviderOptions } from "../api/clientProviders";
import {
  createBrowserDaemonTargetScope,
  createDesktopLocalDaemonTargetScope,
  createDesktopSshDaemonTargetScope,
  createHostOwnerScope,
  createProviderAuthScopeFromOptions,
  createProviderInstallScope,
  createProvisioningScope,
  createWorkspaceOwnerScope,
  daemonTargetScopeFromDesktopConnectionInfo,
  deserializeDaemonTargetScope,
  deserializeOwnerScope,
  deserializeProviderAuthScope,
  deserializeProviderInstallScope,
  deserializeProvisioningScope,
  sameDaemonTargetScope,
  sameOwnerScope,
  sameProviderAuthScope,
  sameProviderInstallScope,
  sameProvisioningScope,
  serializeDaemonTargetScope,
  serializeOwnerScope,
  serializeProviderAuthScope,
  serializeProviderInstallScope,
  serializeProvisioningScope,
} from "./scopeIdentity";

const baseProviderOptions = (overrides?: Partial<ProviderOptions>): ProviderOptions => ({
  provider_id: "codex",
  workspace_id: "ws-1",
  supports_load: true,
  auth_required: true,
  probed_at: "2026-03-10T12:00:00.000Z",
  ...overrides,
});

describe("scopeIdentity", () => {
  it("round-trips daemon target scopes and compares SSH identity structurally", () => {
    const browser = createBrowserDaemonTargetScope("https://example.com", "tok_deadbeef");
    const desktopLocal = createDesktopLocalDaemonTargetScope("http://127.0.0.1:4399");
    const ssh = createDesktopSshDaemonTargetScope({
      host: "host-a.example",
      user: "alice",
      port: 4399,
      dataDir: "/srv/ctx-a",
    });

    expect(deserializeDaemonTargetScope(serializeDaemonTargetScope(browser))).toEqual(browser);
    expect(deserializeDaemonTargetScope(serializeDaemonTargetScope(desktopLocal))).toEqual(desktopLocal);
    expect(deserializeDaemonTargetScope(serializeDaemonTargetScope(ssh))).toEqual(ssh);
    expect(sameDaemonTargetScope(
      ssh,
      createDesktopSshDaemonTargetScope({
        host: "host-a.example",
        user: "alice",
        port: 4399,
        dataDir: "/srv/ctx-a",
      }),
    )).toBe(true);
    expect(sameDaemonTargetScope(
      ssh,
      createDesktopSshDaemonTargetScope({
        host: "host-b.example",
        user: "alice",
        port: 4399,
        dataDir: "/srv/ctx-a",
      }),
    )).toBe(false);
    expect(sameDaemonTargetScope(
      ssh,
      createDesktopSshDaemonTargetScope({
        host: "host-a.example",
        user: "alice",
        port: 4400,
        dataDir: "/srv/ctx-a",
      }),
    )).toBe(false);
    expect(sameDaemonTargetScope(ssh, desktopLocal)).toBe(false);
  });

  it("round-trips owner scopes and keeps host and workspace scopes distinct", () => {
    const daemon = createDesktopSshDaemonTargetScope({
      host: "host-a.example",
      user: "alice",
      port: 4399,
      dataDir: "/srv/ctx-a",
    });
    const hostOwner = createHostOwnerScope(daemon);
    const workspaceOwner = createWorkspaceOwnerScope(daemon, "ws-1");

    expect(deserializeOwnerScope(serializeOwnerScope(hostOwner))).toEqual(hostOwner);
    expect(deserializeOwnerScope(serializeOwnerScope(workspaceOwner))).toEqual(workspaceOwner);
    expect(sameOwnerScope(hostOwner, workspaceOwner)).toBe(false);
    expect(sameOwnerScope(
      workspaceOwner,
      createWorkspaceOwnerScope(daemon, "ws-2"),
    )).toBe(false);
  });

  it("round-trips provider install scopes and includes install target in equality", () => {
    const owner = createWorkspaceOwnerScope(createBrowserDaemonTargetScope("https://example.com"), "ws-1");
    const hostInstall = createProviderInstallScope({
      owner,
      providerId: "codex",
      installTarget: "host",
    });
    const containerInstall = createProviderInstallScope({
      owner,
      providerId: "codex",
      installTarget: "container",
    });

    expect(deserializeProviderInstallScope(serializeProviderInstallScope(hostInstall))).toEqual(hostInstall);
    expect(sameProviderInstallScope(hostInstall, hostInstall)).toBe(true);
    expect(sameProviderInstallScope(hostInstall, containerInstall)).toBe(false);
  });

  it("derives provider auth scope identity from the selected endpoint fingerprint", () => {
    const owner = createWorkspaceOwnerScope(createBrowserDaemonTargetScope("https://example.com"), "ws-1");
    const initial = createProviderAuthScopeFromOptions(owner, "codex", baseProviderOptions({
      auth_mode: "endpoint",
      account_identity: "acct-1",
      source: {
        provider_id: "codex",
        selected_source_kind: "endpoint",
        selected_endpoint_id: "ep-1",
        endpoints: [
          {
            id: "ep-1",
            provider_id: "codex",
            name: "Primary",
            base_url: "https://endpoint-a.example",
            api_shape: "openai_responses",
            auth_type: "api_key",
            model_override: "gpt-5",
            created_at: "2026-03-10T12:00:00.000Z",
            updated_at: "2026-03-10T12:00:00.000Z",
            last_verification_status: "valid",
            has_api_key: true,
          },
        ],
      },
    }));
    const sameScope = createProviderAuthScopeFromOptions(owner, "codex", baseProviderOptions({
      auth_mode: "endpoint",
      account_identity: "acct-1",
      source: {
        provider_id: "codex",
        selected_source_kind: "endpoint",
        selected_endpoint_id: "ep-1",
        endpoints: [
          {
            id: "ep-1",
            provider_id: "codex",
            name: "Primary",
            base_url: "https://endpoint-a.example",
            api_shape: "openai_responses",
            auth_type: "api_key",
            model_override: "gpt-5",
            created_at: "2026-03-10T12:00:00.000Z",
            updated_at: "2026-03-10T12:00:00.000Z",
            last_verification_status: "valid",
            has_api_key: true,
          },
        ],
      },
    }));
    const changedEndpoint = createProviderAuthScopeFromOptions(owner, "codex", baseProviderOptions({
      auth_mode: "endpoint",
      account_identity: "acct-1",
      source: {
        provider_id: "codex",
        selected_source_kind: "endpoint",
        selected_endpoint_id: "ep-1",
        endpoints: [
          {
            id: "ep-1",
            provider_id: "codex",
            name: "Primary",
            base_url: "https://endpoint-b.example",
            api_shape: "openai_responses",
            auth_type: "api_key",
            model_override: "gpt-5",
            created_at: "2026-03-10T12:00:00.000Z",
            updated_at: "2026-03-10T12:05:00.000Z",
            last_verification_status: "valid",
            has_api_key: true,
          },
        ],
      },
    }));

    expect(deserializeProviderAuthScope(serializeProviderAuthScope(initial))).toEqual(initial);
    expect(sameProviderAuthScope(initial, sameScope)).toBe(true);
    expect(sameProviderAuthScope(initial, changedEndpoint)).toBe(false);
  });

  it("round-trips provisioning scopes and parses desktop bridge target metadata", () => {
    const daemon = daemonTargetScopeFromDesktopConnectionInfo({
      kind: "ssh",
      host: "host-a.example",
      user: "alice",
      remote_port: 4399,
      remote_data_dir: "/srv/ctx-a",
    });

    expect(daemon).toEqual(createDesktopSshDaemonTargetScope({
      host: "host-a.example",
      user: "alice",
      port: 4399,
      dataDir: "/srv/ctx-a",
    }));

    const provisioning = createProvisioningScope(
      daemon ?? createDesktopLocalDaemonTargetScope(),
      "host",
    );
    expect(deserializeProvisioningScope(serializeProvisioningScope(provisioning))).toEqual(provisioning);
    expect(sameProvisioningScope(
      provisioning,
      createProvisioningScope(createDesktopLocalDaemonTargetScope(), "host"),
    )).toBe(false);
    expect(daemonTargetScopeFromDesktopConnectionInfo({ kind: "local" })).toEqual(createDesktopLocalDaemonTargetScope());
  });
});
