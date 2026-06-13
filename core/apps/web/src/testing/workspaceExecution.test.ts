import { describe, expect, it } from "vitest";
import type { APIRequestContext } from "playwright/test";

import { ensureLocalLinuxSandboxPrepared } from "../../e2e/utils/workspaceExecution";

type MockResponse = {
  ok: () => boolean;
  status: () => number;
  text: () => Promise<string>;
  json: () => Promise<unknown>;
};

const response = (opts: {
  ok: boolean;
  status: number;
  text?: string;
  json?: unknown;
}): MockResponse => ({
  ok: () => opts.ok,
  status: () => opts.status,
  text: async () => opts.text ?? "",
  json: async () => opts.json ?? {},
});

const requestWithResponse = (resp: MockResponse): APIRequestContext =>
  ({
    post: async () => resp,
  }) as unknown as APIRequestContext;

describe("ensureLocalLinuxSandboxPrepared", () => {
  it("returns when prepare reports ready", async () => {
    await expect(
      ensureLocalLinuxSandboxPrepared(
        requestWithResponse(
          response({
            ok: true,
            status: 200,
            json: {
              ready: true,
              needs_password: false,
              message: "ready",
            },
          }),
        ),
      ),
    ).resolves.toBeUndefined();
  });

  it("fails fast when prepare needs a password", async () => {
    await expect(
      ensureLocalLinuxSandboxPrepared(
        requestWithResponse(
          response({
            ok: true,
            status: 200,
            json: {
              ready: false,
              needs_password: true,
              message: "password required",
            },
          }),
        ),
      ),
    ).rejects.toThrow(/requires admin password/i);
  });

  it("surfaces non-200 prepare failures", async () => {
    await expect(
      ensureLocalLinuxSandboxPrepared(
        requestWithResponse(
          response({
            ok: false,
            status: 500,
            text: "boom",
          }),
        ),
      ),
    ).rejects.toThrow(/linux sandbox prepare failed \(500\): boom/i);
  });
});
