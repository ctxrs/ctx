import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { setWorkerClientConfig, workerFetchJson } from "./workerClient";

describe("workerClient", () => {
  const fetchMock = vi.fn();
  const originalCryptoDesc = Object.getOwnPropertyDescriptor(globalThis, "crypto");
  const originalFetchDesc = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  const mutableGlobal = globalThis as {
    fetch?: typeof fetch;
    crypto?: Crypto;
  };
  let didStubCrypto = false;

  beforeEach(() => {
    fetchMock.mockReset();
    Object.defineProperty(globalThis, "fetch", {
      value: fetchMock,
      configurable: true,
      writable: true,
    });

    didStubCrypto = false;
    if (!globalThis.crypto?.getRandomValues) {
      didStubCrypto = true;
      Object.defineProperty(globalThis, "crypto", {
        value: {
          getRandomValues: (arr: Uint8Array) => {
            for (let i = 0; i < arr.length; i += 1) {
              arr[i] = (i + 1) % 255;
            }
            return arr;
          },
        },
        configurable: true,
      });
    }
  });

  afterEach(() => {
    fetchMock.mockReset();
    if (originalFetchDesc) {
      Object.defineProperty(globalThis, "fetch", originalFetchDesc);
    } else {
      // Best-effort cleanup for environments where fetch isn't a real global.
      // eslint-disable-next-line @typescript-eslint/no-dynamic-delete
      delete mutableGlobal.fetch;
    }

    if (didStubCrypto) {
      if (originalCryptoDesc) {
        Object.defineProperty(globalThis, "crypto", originalCryptoDesc);
      } else {
        // eslint-disable-next-line @typescript-eslint/no-dynamic-delete
        delete mutableGlobal.crypto;
      }
    }
  });

  it("adds base URL and headers", async () => {
    setWorkerClientConfig({
      baseUrl: "http://daemon:4399/",
      authToken: "token-1",
      runId: "run-1",
    });

    fetchMock.mockResolvedValue({
      ok: true,
      status: 200,
      headers: new Headers({ "content-type": "application/json" }),
      text: async () => JSON.stringify({ ok: true }),
    });

    const result = await workerFetchJson<{ ok: boolean }>("/api/health");

    expect(result.ok).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://daemon:4399/api/health");

    const headers = (init as RequestInit).headers as Record<string, string>;
    expect(headers["content-type"]).toBe("application/json");
    expect(headers.authorization).toBe("Bearer token-1");
    expect(headers["x-ctx-run-id"]).toBe("run-1");
    expect(headers.traceparent).toMatch(/^00-[0-9a-f]{32}-[0-9a-f]{16}-01$/);
  });

  it("preserves auth/trace headers when init.headers is provided", async () => {
    setWorkerClientConfig({
      baseUrl: "http://daemon:4399/",
      authToken: "token-2",
      runId: "run-2",
    });

    fetchMock.mockResolvedValue({
      ok: true,
      status: 200,
      headers: new Headers({ "content-type": "application/json" }),
      text: async () => JSON.stringify({ ok: true }),
    });

    const result = await workerFetchJson<{ ok: boolean }>("/api/health", {
      method: "POST",
      headers: {
        "x-custom": "1",
      },
    });

    expect(result.ok).toBe(true);
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://daemon:4399/api/health");

    const headers = (init as RequestInit).headers as Record<string, string>;
    expect(headers["x-custom"]).toBe("1");
    expect(headers.authorization).toBe("Bearer token-2");
    expect(headers["x-ctx-run-id"]).toBe("run-2");
    expect(headers.traceparent).toMatch(/^00-[0-9a-f]{32}-[0-9a-f]{16}-01$/);
  });
});
