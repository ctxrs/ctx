import { test as base, expect, chromium } from "playwright/test";
import {
  collectPageFailureDiagnosticBundleInput,
  writeE2EDiagnosticBundleForFailure,
} from "./utils/diagnostics";

const AUTH_TOKEN = process.env.CTX_E2E_AUTH_TOKEN ?? "ctx-e2e-auth-token";
const MAX_BROWSER_CONSOLE_TAIL = 200;
const MAX_BROWSER_CONSOLE_ENTRY_LENGTH = 2000;

type E2EWindow = Window & {
  __ctxE2E?: {
    getOpenedWebSocketUrls?: () => string[];
    clearOpenedWebSocketUrls?: () => void;
  };
};

const test = base.extend({
  context: async ({ context }, use) => {
    await context.addInitScript((token: string) => {
      const wsUrls: string[] = [];
      const OriginalWebSocket = window.WebSocket;
      if (typeof OriginalWebSocket === "function") {
        class TrackedWebSocket extends OriginalWebSocket {
          constructor(url: string | URL, protocols?: string | string[]) {
            wsUrls.push(String(url));
            super(url, protocols as string | string[] | undefined);
          }
        }
        for (const key of Object.getOwnPropertyNames(OriginalWebSocket)) {
          if (Object.prototype.hasOwnProperty.call(TrackedWebSocket, key)) continue;
          try {
            Object.defineProperty(TrackedWebSocket, key, {
              value: Reflect.get(OriginalWebSocket as object, key),
              configurable: true,
              writable: true,
            });
          } catch {
            // ignore define errors
          }
        }
        Object.defineProperty(window, "WebSocket", {
          value: TrackedWebSocket,
          configurable: true,
          writable: true,
        });
      }
      window.sessionStorage.setItem(
        "ctxDaemonConnectionV1",
        JSON.stringify({
          v: 1,
          baseUrl: window.location.origin,
          wsBaseUrl: window.location.origin.replace(/^http/, "ws"),
          authToken: token,
          source: "e2e_init",
        }),
      );
      // Used by a few tests to enable app-side E2E hooks (disabled in normal usage).
      window.sessionStorage.setItem("ctxE2E", "1");
      const w = window as E2EWindow;
      w.__ctxE2E ??= {};
      w.__ctxE2E.getOpenedWebSocketUrls = () => wsUrls.slice();
      w.__ctxE2E.clearOpenedWebSocketUrls = () => {
        wsUrls.length = 0;
      };
    }, AUTH_TOKEN);
    await use(context);
  },
  page: async ({ page }, use, testInfo) => {
    const browserConsoleTail: string[] = [];
    page.on("console", (message) => {
      browserConsoleTail.push(`${message.type()}: ${message.text()}`.slice(0, MAX_BROWSER_CONSOLE_ENTRY_LENGTH));
      while (browserConsoleTail.length > MAX_BROWSER_CONSOLE_TAIL) {
        browserConsoleTail.shift();
      }
    });
    await use(page);
    if (testInfo.status !== testInfo.expectedStatus) {
      const bundle = await collectPageFailureDiagnosticBundleInput({
        browserConsoleTail,
        failurePhase: "test_failure",
        launchState: "invoked",
        page,
        testInfo,
      });
      await writeE2EDiagnosticBundleForFailure({
        bundle,
        testInfo,
      });
    }
  },
});

export { test, expect, chromium };
