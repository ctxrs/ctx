import { beforeEach, describe, expect, it } from "vitest";
import { setDaemonConnection } from "../api/daemonConnection";
import {
  createDesktopLocalDaemonTargetScope,
} from "./scopeIdentity";
import {
  getProviderOwnerScopeKey,
} from "./providerScopeAdapters";

describe("providerScopeAdapters", () => {
  beforeEach(() => {
    setDaemonConnection({
      baseUrl: "https://daemon-a.example",
      authToken: null,
      source: "test",
      targetScope: null,
    });
  });

  it("distinguishes desktop-local daemon owner scopes by baseUrl", () => {
    setDaemonConnection({
      baseUrl: "http://127.0.0.1:4399",
      authToken: "token-a",
      source: "desktop",
      targetScope: createDesktopLocalDaemonTargetScope(),
    });
    const first = getProviderOwnerScopeKey("ws-1");

    setDaemonConnection({
      baseUrl: "http://127.0.0.1:4400",
      authToken: "token-b",
      source: "desktop",
      targetScope: createDesktopLocalDaemonTargetScope(),
    });
    const second = getProviderOwnerScopeKey("ws-1");

    expect(first).not.toBe(second);
  });
});
