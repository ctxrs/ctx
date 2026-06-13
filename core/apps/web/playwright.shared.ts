import { defineConfig, type PlaywrightTestConfig } from "playwright/test";
import crypto from "crypto";
import fs from "fs";
import net from "net";
import path from "path";
import { createRequire } from "module";
import { fileURLToPath } from "url";
import { parseBoolishString } from "./src/utils/boolish";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const require = createRequire(import.meta.url);
const {
  buildCtxCacheEnv,
  resolveCtxCacheLayout,
} = require("../../scripts/lib/cache_roots.cjs");

const HOST = "127.0.0.1";
const DEFAULT_PORT = 4401;

const parseBool = (value?: string) => parseBoolishString(value) === true;

const hasValue = (value?: string) => Boolean(value?.trim());

const repoHash = crypto.createHash("sha1").update(path.resolve(__dirname, "../..")).digest("hex").slice(0, 10);

const resolveConfiguredPath = (configured: string) => {
  return path.isAbsolute(configured)
    ? configured
    : path.resolve(__dirname, "../..", configured);
};

const resolveCacheLayout = (env: NodeJS.ProcessEnv) =>
  resolveCtxCacheLayout({
    cwd: path.resolve(__dirname, "../.."),
    env,
  });

const resolveWorkers = (defaultWorkers: number | undefined) => {
  const raw = String(process.env.CTX_E2E_WORKERS ?? process.env.PW_WORKERS ?? "").trim();
  if (!raw) return defaultWorkers;
  if (raw.toLowerCase() === "auto") return undefined;
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? n : defaultWorkers;
};

const resolveWebServerStdio = () => {
  const raw = String(process.env.CTX_E2E_WEB_SERVER_STDIO ?? "").trim().toLowerCase();
  if (!raw) return {} as const;
  if (raw === "ignore") return { stdout: "ignore" as const, stderr: "ignore" as const };
  if (raw === "pipe") return { stdout: "pipe" as const, stderr: "pipe" as const };
  return {} as const;
};

const buildWebServerBaseEnv = (env: NodeJS.ProcessEnv) => {
  return Object.fromEntries(
    Object.entries(env).filter(([key]) => {
      if (!key.startsWith("CTX_")) return true;
      if (key.startsWith("CTX_E2E_") || key.startsWith("CTX_VOLATILE_")) return true;
      return key === "CTX_MCP_COMMAND";
    }),
  );
};

export const applyProcessEnvDefaultIfPresent = (
  env: NodeJS.ProcessEnv,
  key: string,
  value: string | undefined,
) => {
  const normalized = String(value ?? "").trim();
  if (!normalized) {
    return;
  }
  env[key] ??= normalized;
};

export const resolvePlaywrightCargoTargetDir = (env: NodeJS.ProcessEnv) => {
  const configured = String(env.CTX_E2E_CARGO_TARGET_DIR ?? env.CARGO_TARGET_DIR ?? "").trim();
  if (configured) {
    return resolveConfiguredPath(configured);
  }
  const cacheLayout = resolveCacheLayout(env);
  return path.join(
    cacheLayout.targetsDir,
    "ctx-e2e",
    `e2e-${repoHash}`,
  );
};

const resolvePort = async (reuseExistingServer: boolean): Promise<number> => {
  const requestedPort = Number(process.env.CTX_E2E_PORT);
  if (Number.isFinite(requestedPort) && requestedPort > 0) {
    return requestedPort;
  }
  if (reuseExistingServer) {
    return DEFAULT_PORT;
  }
  return await new Promise<number>((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.on("error", reject);
    server.listen(0, HOST, () => {
      const address = server.address();
      if (address && typeof address === "object") {
        const { port } = address;
        server.close(() => resolve(port));
        return;
      }
      server.close(() => reject(new Error("Failed to resolve free port")));
    });
  });
};

export type E2ESuiteProfile =
  | "all"
  | "premerge_required"
  | "release_required"
  | "cross_platform"
  | "visual"
  | "soak"
  | "load";

type PlaywrightBrowserName = "chromium" | "firefox" | "webkit";

type CtxPlaywrightServerMode = "managed" | "external";

type CtxPlaywrightConfigOptions = {
  serverMode?: CtxPlaywrightServerMode;
  baseURL?: string;
  authToken?: string;
  ignoreHTTPSErrors?: boolean;
};

const resolvePlaywrightBrowserName = (): PlaywrightBrowserName => {
  const raw = String(process.env.CTX_E2E_BROWSER ?? "").trim().toLowerCase();
  if (!raw) return "webkit";
  if (raw === "chromium" || raw === "firefox" || raw === "webkit") {
    return raw;
  }
  throw new Error(
    `Unsupported CTX_E2E_BROWSER '${raw}'. Expected one of: chromium, firefox, webkit.`,
  );
};

const resolveExternalBaseURL = (configuredBaseURL?: string): string => {
  const configured = String(configuredBaseURL ?? process.env.CTX_E2E_BASE_URL ?? "").trim();
  if (configured) {
    return configured;
  }
  const portText = String(process.env.CTX_E2E_PORT ?? "").trim();
  const port = Number(portText);
  const resolvedPort = Number.isFinite(port) && port > 0 ? port : DEFAULT_PORT;
  return `http://${HOST}:${resolvedPort}`;
};

export async function createCtxPlaywrightConfig(
  profile: E2ESuiteProfile,
  {
    serverMode = "managed",
    baseURL: configuredBaseURL,
    authToken: configuredAuthToken,
    ignoreHTTPSErrors,
  }: CtxPlaywrightConfigOptions = {},
): Promise<PlaywrightTestConfig> {
  const profileSlug = profile.replace(/[^a-z0-9_-]/gi, "-").toLowerCase();
  const reuseRequested = parseBool(process.env.CTX_E2E_REUSE_SERVER);
  const forceReuseExistingServer = parseBool(process.env.CTX_E2E_FORCE_REUSE_SERVER);
  const reuseExistingServer =
    forceReuseExistingServer
      ? true
      : profile === "premerge_required" || profile === "release_required"
        ? false
        : reuseRequested;
  const skipWebBuild = parseBool(process.env.CTX_E2E_SKIP_WEB_BUILD);
  const AUTH_TOKEN = configuredAuthToken ?? process.env.CTX_E2E_AUTH_TOKEN ?? "ctx-e2e-auth-token";
  const managedPort = serverMode === "managed" ? await resolvePort(reuseExistingServer) : null;
  if (managedPort != null) {
    process.env.CTX_E2E_PORT = String(managedPort);
  }
  const baseURL =
    serverMode === "managed"
      ? `http://${HOST}:${managedPort}`
      : resolveExternalBaseURL(configuredBaseURL);
  const readinessURL = `${baseURL}/api/health`;

  const { env: cacheEnv, layout: cacheLayout } = buildCtxCacheEnv({
    cwd: path.resolve(__dirname, "../.."),
    env: process.env,
    mode: "workspace",
  });
  const volatileTmpRoot =
    process.env.CTX_E2E_TMPDIR
    ?? process.env.CTX_VOLATILE_TMPDIR
    ?? cacheLayout.tmpDir;
  const defaultTmpDir = path.join(volatileTmpRoot, `ctx-e2e-${profileSlug}-tmp-${process.pid}`);
  const defaultDataDir = path.join(volatileTmpRoot, `ctx-e2e-${profileSlug}-data-${process.pid}`);
  const tmpDir = process.env.CTX_E2E_TMPDIR ?? defaultTmpDir;
  const dataDir = process.env.CTX_E2E_DATA_DIR ?? defaultDataDir;
  applyProcessEnvDefaultIfPresent(process.env, "CARGO_HOME", cacheEnv.CARGO_HOME);
  applyProcessEnvDefaultIfPresent(process.env, "SCCACHE_DIR", cacheEnv.SCCACHE_DIR);
  applyProcessEnvDefaultIfPresent(process.env, "SCCACHE_PATH", cacheEnv.SCCACHE_PATH);
  applyProcessEnvDefaultIfPresent(process.env, "RUSTC_WRAPPER", cacheEnv.RUSTC_WRAPPER);
  applyProcessEnvDefaultIfPresent(process.env, "CTX_VOLATILE_ROOT", cacheEnv.CTX_VOLATILE_ROOT);
  applyProcessEnvDefaultIfPresent(process.env, "CTX_VOLATILE_ROOT_MODE", cacheEnv.CTX_VOLATILE_ROOT_MODE);
  applyProcessEnvDefaultIfPresent(process.env, "CTX_VOLATILE_TARGETS_DIR", cacheEnv.CTX_VOLATILE_TARGETS_DIR);
  applyProcessEnvDefaultIfPresent(
    process.env,
    "CTX_VOLATILE_ARTIFACTS_DIR",
    cacheEnv.CTX_VOLATILE_ARTIFACTS_DIR,
  );
  process.env.CTX_E2E_TMPDIR ??= tmpDir;
  process.env.CTX_E2E_DATA_DIR ??= dataDir;
  process.env.CTX_VOLATILE_TMPDIR ??= volatileTmpRoot;
  process.env.CTX_E2E_AUTH_TOKEN ??= AUTH_TOKEN;
  applyProcessEnvDefaultIfPresent(
    process.env,
    "PLAYWRIGHT_BROWSERS_PATH",
    cacheEnv.PLAYWRIGHT_BROWSERS_PATH,
  );
  const defaultBundleDir = path.resolve(__dirname, "../desktop/src-tauri/bundles");
  const bundleManifestPath = path.join(defaultBundleDir, "manifest.json");
  const allowConfiguredBundleDir = parseBool(process.env.CTX_E2E_ALLOW_CONFIGURED_BUNDLE_DIR);
  const allowDefaultBundleDir =
    String(process.env.CTX_E2E_RUNTIME_SOURCE ?? "").trim() !== "bazel-runfiles";
  const resolvedBundleDir =
    (process.env.CTX_E2E_BUNDLE_DIR
      ?? (allowConfiguredBundleDir ? process.env.CTX_BUNDLE_DIR : "")
      ?? "")
      .trim()
    || (allowDefaultBundleDir && fs.existsSync(bundleManifestPath) ? defaultBundleDir : "");
  if (resolvedBundleDir) {
    process.env.CTX_E2E_BUNDLED_ONLY ??= "1";
  } else {
    delete process.env.CTX_BUNDLE_DIR;
  }

  const docsMirrorBin = path.resolve(__dirname, "e2e/fixtures/ctx-docs-mirror-fixture.sh");
  const cargoTargetDir = resolvePlaywrightCargoTargetDir(process.env);
  const cargoIncremental = String(process.env.CARGO_INCREMENTAL ?? "").trim() || "0";

  const bazelOutputRoot = String(process.env.TEST_UNDECLARED_OUTPUTS_DIR ?? "").trim();
  const outputDir = bazelOutputRoot
    ? path.resolve(bazelOutputRoot, "playwright", profileSlug, "test-results")
    : path.resolve(__dirname, `e2e/test-results/${profileSlug}`);
  const reportDir = bazelOutputRoot
    ? path.resolve(bazelOutputRoot, "playwright", profileSlug, "playwright-report")
    : path.resolve(__dirname, `e2e/playwright-report/${profileSlug}`);
  const primaryReporter = process.env.CTX_E2E_REPORTER ?? "dot";
  const browserName = resolvePlaywrightBrowserName();
  const argosEnabled =
    parseBool(process.env.CTX_E2E_ARGOS) || hasValue(process.env.ARGOS_TOKEN);
  const reporter: PlaywrightTestConfig["reporter"] = [
    [primaryReporter],
    ["html", { outputFolder: reportDir, open: "never" }],
  ];
  if (argosEnabled) {
    reporter.push([
      "@argos-ci/playwright/reporter",
      {
        uploadToArgos: true,
        buildName: `ctx-web-${profileSlug}`,
      },
    ]);
  }
  const webServerEnv = serverMode === "managed"
    ? {
      ...buildWebServerBaseEnv(process.env),
      CTX_E2E_DATA_DIR: dataDir,
      CTX_E2E_TMPDIR: tmpDir,
      CTX_VOLATILE_TMPDIR: volatileTmpRoot,
      CTX_E2E_AUTH_TOKEN: AUTH_TOKEN,
      CTX_E2E_SKIP_WEB_BUILD: skipWebBuild ? "1" : "0",
      CTX_E2E_HOST: HOST,
      CTX_E2E_PORT: String(managedPort),
      CTX_DOCS_MIRROR_BIN: docsMirrorBin,
      CTX_E2E_CARGO_TARGET_DIR: cargoTargetDir,
      CARGO_TARGET_DIR: cargoTargetDir,
      CARGO_INCREMENTAL: cargoIncremental,
      TMPDIR: tmpDir,
      TMP: tmpDir,
      TEMP: tmpDir,
      CTX_EXECUTION_MODE: "host",
      CTX_SHOW_FAKE_PROVIDER: "1",
      CTX_DEV_MODE: "1",
      CTX_STORAGE_BACKEND: "sqlite",
      CTX_WEB_DIST: "",
      ...(process.platform === "win32" ? {} : { SHELL: "/bin/sh" }),
    }
    : null;
  if (resolvedBundleDir && webServerEnv) {
    webServerEnv.CTX_BUNDLE_DIR = resolvedBundleDir;
    webServerEnv.CTX_BUNDLE_MANIFEST = path.join(resolvedBundleDir, "manifest.json");
    webServerEnv.CTX_E2E_BUNDLED_ONLY ??= "1";
  }

  return defineConfig({
    testDir: "./e2e",
    timeout: 60_000,
    workers: resolveWorkers(1),
    outputDir,
    reporter,
    use: {
      browserName,
      baseURL,
      extraHTTPHeaders: {
        authorization: `Bearer ${AUTH_TOKEN}`,
      },
      headless: true,
      ignoreHTTPSErrors: ignoreHTTPSErrors ?? baseURL.startsWith("https://"),
      screenshot: "only-on-failure",
      trace: "retain-on-failure",
      video: "retain-on-failure",
    },
    webServer: webServerEnv == null
      ? undefined
      : {
        url: readinessURL,
        command: "node apps/web/scripts/start-e2e-server.mjs",
        cwd: "../..",
        env: webServerEnv,
        reuseExistingServer,
        timeout: 1_200_000,
        ...resolveWebServerStdio(),
      },
  });
}
