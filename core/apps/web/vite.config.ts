import fs from "fs";
import os from "os";
import path from "path";
import { fileURLToPath } from "url";
import { defineConfig } from "vite";
import type { Plugin } from "vite";
import react from "@vitejs/plugin-react";
import mkcert from "vite-plugin-mkcert";
import { validateProductionAnalyticsBuildConfig } from "./src/utils/analytics/config";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const packageJsonPath = path.join(__dirname, "package.json");
const packageJson = JSON.parse(fs.readFileSync(packageJsonPath, "utf8")) as { version?: string };

type DaemonAuthFile = {
  token?: string;
  daemon_url?: string;
};

const loadDaemonAuth = (): DaemonAuthFile | null => {
  const dataDir = process.env.CTX_DATA_DIR ?? path.join(os.homedir(), ".ctx");
  const authPath = path.join(dataDir, "daemon_auth.json");
  try {
    const raw = fs.readFileSync(authPath, "utf8");
    return JSON.parse(raw) as DaemonAuthFile;
  } catch {
    return null;
  }
};
const httpsHosts = String(process.env.CTX_DEV_HTTPS_HOSTS ?? "")
  .split(",")
  .map((host) => host.trim())
  .filter(Boolean);

const proxyDaemonAuthToken =
  process.env.CTX_DEV_PROXY_DAEMON_AUTH === "1"
    ? (process.env.CTX_DEV_PROXY_DAEMON_AUTH_TOKEN ?? null)
    : null;

const WAL_ROUTE = "/__ctx_wal__";
const WAL_MAX_BYTES = 5 * 1024 * 1024;

const walPlugin = (enabled: boolean): Plugin => ({
  name: "ctx-web-wal",
  configureServer(server) {
    if (!enabled) return;
    const walDir = process.env.CTX_WEB_WAL_DIR ?? path.join(os.tmpdir(), "ctx-web-wal");
    fs.mkdirSync(walDir, { recursive: true });
    const sessions = new Map<
      string,
      { path: string; stream: fs.WriteStream; bytes: number; lastWriteMs: number }
    >();
    const stamp = new Date().toISOString().replace(/[:.]/g, "-");

    const getSession = (sessionId: string) => {
      const existing = sessions.get(sessionId);
      if (existing) return existing;
      const filePath = path.join(walDir, `ctx-web-wal-${stamp}-${sessionId}.jsonl`);
      const stream = fs.createWriteStream(filePath, { flags: "a" });
      const session = { path: filePath, stream, bytes: 0, lastWriteMs: 0 };
      sessions.set(sessionId, session);
      server.config.logger.info(`[ctx] web wal (${sessionId}): ${filePath}`);
      return session;
    };

    server.config.logger.info(`[ctx] web wal dir: ${walDir}`);

    server.middlewares.use((req, res, next) => {
      const rawUrl = req.url ?? "";
      if (!rawUrl.startsWith(WAL_ROUTE)) return next();
      const url = new URL(rawUrl, "http://localhost");

      if (req.method === "GET" && url.pathname === `${WAL_ROUTE}/status`) {
        const sessionId = url.searchParams.get("session") ?? "";
        if (!sessionId) {
          res.statusCode = 400;
          res.end("missing session");
          return;
        }
        const session = sessions.get(sessionId);
        res.setHeader("content-type", "application/json");
        res.statusCode = 200;
        res.end(
          JSON.stringify({
            ok: Boolean(session),
            session: sessionId,
            path: session?.path ?? null,
            bytes: session?.bytes ?? 0,
            last_write_ms: session?.lastWriteMs ?? 0,
          }),
        );
        return;
      }

      if (req.method !== "POST") {
        res.statusCode = 405;
        res.end("method not allowed");
        return;
      }

      const sessionId = url.searchParams.get("session") ?? String(req.headers["x-ctx-wal-session"] ?? "");
      if (!sessionId) {
        res.statusCode = 400;
        res.end("missing session");
        return;
      }

      let size = 0;
      let body = "";
      req.setEncoding("utf8");
      req.on("data", (chunk) => {
        size += chunk.length;
        if (size > WAL_MAX_BYTES) {
          res.statusCode = 413;
          res.end("payload too large");
          req.destroy();
          return;
        }
        body += chunk;
      });
      req.on("end", () => {
        const session = getSession(sessionId);
        session.stream.write(body);
        session.bytes += Buffer.byteLength(body);
        session.lastWriteMs = Date.now();
        res.statusCode = 204;
        res.end();
      });
    });

    server.httpServer?.once("close", () => {
      for (const session of sessions.values()) {
        session.stream.end();
      }
    });
  },
});

export default defineConfig(({ command }) => {
  const isTest = process.env.VITEST === "true" || process.env.NODE_ENV === "test";
  validateProductionAnalyticsBuildConfig({
    explicitAnalyticsEnv: process.env.VITE_POSTHOG_ENV,
    explicitAppVersion: process.env.VITE_CTX_APP_VERSION,
    packageVersion: packageJson.version,
  });
  const auth = command === "serve" ? loadDaemonAuth() : null;
  const daemonUrl =
    process.env.CTX_DAEMON_URL ?? auth?.daemon_url ?? "http://127.0.0.1:4399";
  const devPort = Number(process.env.CTX_WEB_PORT ?? 5173);
  const useHttps =
    process.env.CTX_DEV_HTTPS === "1"
      ? true
      : process.env.CTX_DEV_HTTP === "1"
        ? false
        : false;
  const appVersion = String(process.env.VITE_CTX_APP_VERSION ?? packageJson.version ?? "0.0.0");
  const isCi = ["1", "true", "yes", "on"].includes(
    String(process.env.CI ?? "").trim().toLowerCase(),
  );

  if (command === "serve" && auth?.token) {
    process.env.VITE_CTX_AUTH_TOKEN ??= auth.token;
    process.env.VITE_CTX_DAEMON_URL ??= daemonUrl;
  }

  const devProxyAuthToken =
    command === "serve" && !isTest && process.env.CTX_DEV_PROXY_DAEMON_AUTH === "1"
      ? (proxyDaemonAuthToken ?? auth?.token ?? process.env.VITE_CTX_AUTH_TOKEN ?? null)
      : null;

  const withDaemonAuthHeader = <T extends { configure?: (proxy: any, options: any) => void }>(
    config: T,
  ): T => {
    if (!devProxyAuthToken) return config;

    return {
      ...config,
      configure(proxy, options) {
        proxy.on("proxyReq", (proxyReq: any) => {
          if (!proxyReq.getHeader("authorization")) {
            proxyReq.setHeader("authorization", `Bearer ${devProxyAuthToken}`);
          }
        });
        proxy.on("proxyReqWs", (proxyReq: any) => {
          if (!proxyReq.getHeader("authorization")) {
            proxyReq.setHeader("authorization", `Bearer ${devProxyAuthToken}`);
          }
        });
        config.configure?.(proxy, options);
      },
    };
  };

  return {
    define: {
      __CTX_APP_VERSION__: JSON.stringify(appVersion),
      __CTX_BUILD_CI__: JSON.stringify(isCi),
    },
    plugins: [
      react(),
      walPlugin(command === "serve" && !isTest),
      ...(command === "serve" && useHttps && !isTest
        ? [mkcert(httpsHosts.length > 0 ? { hosts: httpsHosts } : undefined)]
        : []),
    ],
    worker: {
      format: "es",
    },
    test: {
      globals: true,
      environment: "jsdom",
      setupFiles: "./vitest.setup.ts",
      exclude: ["e2e/**", "node_modules/**"],
      coverage: {
        provider: "v8",
        reporter: ["text", "lcov", "json-summary"],
        reportsDirectory: path.resolve(__dirname, "../../coverage/web"),
      },
    },
    build: {
      // Web shipping is not a current product priority; keep warning output quiet
      // while we focus optimization work on desktop/mobile flows.
      chunkSizeWarningLimit: 2000,
    },
    server: {
      host: "0.0.0.0",
      port: Number.isFinite(devPort) ? devPort : 5173,
      strictPort: true,
      https: useHttps,
      proxy: {
        "/api": withDaemonAuthHeader({
          target: daemonUrl,
          changeOrigin: true,
          ws: true,
          xfwd: true,
        }),
        // Web sessions are served by the daemon; proxy for dev server parity.
        "/sessions": withDaemonAuthHeader({
          target: daemonUrl,
          changeOrigin: true,
          ws: true,
          xfwd: true,
        }),
      },
    },
    preview: {
      host: "0.0.0.0",
      https: useHttps,
    },
  };
});
