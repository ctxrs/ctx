import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import {
  applyProcessEnvDefaultIfPresent,
  createCtxPlaywrightConfig,
  resolvePlaywrightCargoTargetDir,
} from "./playwright.shared";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const ORIGINAL_ENV = { ...process.env };

type ReporterTuple = readonly [string, Record<string, unknown>?];

const restoreEnv = () => {
  process.env = { ...ORIGINAL_ENV };
  delete process.env.CTX_E2E_ARGOS;
  delete process.env.ARGOS_TOKEN;
  delete process.env.CTX_E2E_REPORTER;
  delete process.env.CTX_VOLATILE_ROOT;
  delete process.env.CTX_VOLATILE_ROOT_MODE;
  delete process.env.CTX_VOLATILE_TARGETS_DIR;
  delete process.env.CTX_VOLATILE_ARTIFACTS_DIR;
  delete process.env.CTX_VOLATILE_TMPDIR;
  delete process.env.CTX_E2E_TMPDIR;
  delete process.env.CTX_E2E_DATA_DIR;
  delete process.env.CTX_BUNDLE_DIR;
  delete process.env.CTX_BUNDLE_MANIFEST;
  delete process.env.CTX_WEB_DIST;
  delete process.env.CTX_E2E_BUNDLE_DIR;
  delete process.env.CTX_E2E_ALLOW_CONFIGURED_BUNDLE_DIR;
  delete process.env.CTX_E2E_RUNTIME_SOURCE;
  delete process.env.CTX_E2E_BROWSER;
  delete process.env.CTX_E2E_REUSE_SERVER;
  delete process.env.CTX_E2E_FORCE_REUSE_SERVER;
  delete process.env.PLAYWRIGHT_BROWSERS_PATH;
  delete process.env.CARGO_INCREMENTAL;
  delete process.env.TEST_UNDECLARED_OUTPUTS_DIR;
};

const getReporterTuples = (reporter: unknown): ReporterTuple[] => {
  if (!Array.isArray(reporter)) return [];
  return reporter.filter((entry): entry is ReporterTuple => {
    return Array.isArray(entry) && typeof entry[0] === "string";
  });
};

afterEach(() => {
  restoreEnv();
});

describe("createCtxPlaywrightConfig", () => {
  it("skips undefined cache defaults instead of stringifying them into process.env", async () => {
    restoreEnv();
    delete process.env.RUSTC_WRAPPER;

    applyProcessEnvDefaultIfPresent(process.env, "RUSTC_WRAPPER", undefined);

    expect(process.env.RUSTC_WRAPPER).toBeUndefined();
  });

  it("writes reports under e2e artifact roots", async () => {
    restoreEnv();
    const config = await createCtxPlaywrightConfig("all");
    expect(config.outputDir).toBe(path.resolve(__dirname, "e2e/test-results/all"));
    expect(config.webServer?.url).toMatch(/^http:\/\/127\.0\.0\.1:\d+\/api\/health$/);

    const reporters = getReporterTuples(config.reporter);
    const htmlReporter = reporters.find((entry) => entry[0] === "html");
    expect(htmlReporter?.[1]).toMatchObject({
      outputFolder: path.resolve(__dirname, "e2e/playwright-report/all"),
      open: "never",
    });
  });

  it("writes reports under Bazel undeclared outputs when present", async () => {
    restoreEnv();
    process.env.TEST_UNDECLARED_OUTPUTS_DIR = "/tmp/ctx-bazel-outputs";

    const config = await createCtxPlaywrightConfig("premerge_required");
    expect(config.outputDir).toBe(path.resolve(
      "/tmp/ctx-bazel-outputs",
      "playwright/premerge_required/test-results",
    ));

    const reporters = getReporterTuples(config.reporter);
    const htmlReporter = reporters.find((entry) => entry[0] === "html");
    expect(htmlReporter?.[1]).toMatchObject({
      outputFolder: path.resolve(
        "/tmp/ctx-bazel-outputs",
        "playwright/premerge_required/playwright-report",
      ),
      open: "never",
    });
  });

  it("does not add Argos by default", async () => {
    restoreEnv();
    const config = await createCtxPlaywrightConfig("premerge_required");
    const reporters = getReporterTuples(config.reporter);
    expect(reporters.map((entry) => entry[0])).not.toContain("@argos-ci/playwright/reporter");
  });

  it("adds the Argos reporter when an Argos token is present", async () => {
    restoreEnv();
    process.env.ARGOS_TOKEN = "test-token";
    const config = await createCtxPlaywrightConfig("premerge_required");
    const reporters = getReporterTuples(config.reporter);
    const argosReporter = reporters.find((entry) => entry[0] === "@argos-ci/playwright/reporter");
    expect(argosReporter).toBeTruthy();
    expect(argosReporter?.[1]).toMatchObject({
      uploadToArgos: true,
      buildName: "ctx-web-premerge_required",
    });
  });

  it("defaults the shared browser lane to webkit", async () => {
    restoreEnv();
    const config = await createCtxPlaywrightConfig("premerge_required");
    expect(config.use?.browserName).toBe("webkit");
  });

  it("allows the shared browser lane to be overridden explicitly", async () => {
    restoreEnv();
    process.env.CTX_E2E_BROWSER = "chromium";
    const config = await createCtxPlaywrightConfig("premerge_required");
    expect(config.use?.browserName).toBe("chromium");
  });

  it("defaults e2e cargo builds to a stable cache dir instead of per-run temp dirs", async () => {
    restoreEnv();
    delete process.env.CTX_E2E_CARGO_TARGET_DIR;
    delete process.env.CARGO_TARGET_DIR;

    const resolved = resolvePlaywrightCargoTargetDir(process.env);
    expect(resolved).toContain(path.join("targets", "ctx-e2e"));
    expect(resolved).toContain("e2e-");
    expect(resolved).not.toContain("ctx-e2e-cargo-");
  });

  it("threads the resolved cargo target dir into the webServer env", async () => {
    restoreEnv();
    const config = await createCtxPlaywrightConfig("all");
    const webServer = Array.isArray(config.webServer) ? config.webServer[0] : config.webServer;
    expect(webServer?.env?.CTX_E2E_CARGO_TARGET_DIR).toBe(resolvePlaywrightCargoTargetDir(process.env));
    expect(webServer?.env?.CARGO_TARGET_DIR).toBe(resolvePlaywrightCargoTargetDir(process.env));
    expect(webServer?.env?.CARGO_INCREMENTAL).toBe("0");
  });

  it("defaults e2e tmp and data dirs under separate volatile tmp roots", async () => {
    restoreEnv();
    const config = await createCtxPlaywrightConfig("all");
    const webServer = Array.isArray(config.webServer) ? config.webServer[0] : config.webServer;
    const expectedTmpPrefix = path.join("tmp", "ctx-e2e-all-tmp-");
    const expectedDataPrefix = path.join("tmp", "ctx-e2e-all-data-");

    expect(String(webServer?.env?.CTX_E2E_TMPDIR)).toContain(expectedTmpPrefix);
    expect(String(webServer?.env?.CTX_E2E_DATA_DIR)).toContain(expectedDataPrefix);
    expect(webServer?.env?.CTX_E2E_TMPDIR).not.toBe(webServer?.env?.CTX_E2E_DATA_DIR);
    expect(String(webServer?.env?.CTX_VOLATILE_TMPDIR)).toContain(path.join("volatile", "tmp"));
    expect(webServer?.env?.TMPDIR).toBe(webServer?.env?.CTX_E2E_TMPDIR);
    expect(webServer?.env?.TMP).toBe(webServer?.env?.CTX_E2E_TMPDIR);
    expect(webServer?.env?.TEMP).toBe(webServer?.env?.CTX_E2E_TMPDIR);
  });

  it("threads shared cache layout env into the webServer env", async () => {
    restoreEnv();
    const config = await createCtxPlaywrightConfig("all");
    const webServer = Array.isArray(config.webServer) ? config.webServer[0] : config.webServer;

    expect(String(webServer?.env?.CTX_VOLATILE_ROOT)).toContain(path.join("volatile"));
    expect(["explicit", "preferred-external", "internal-fallback"]).toContain(String(webServer?.env?.CTX_VOLATILE_ROOT_MODE));
    expect(String(webServer?.env?.CTX_VOLATILE_ARTIFACTS_DIR)).toContain(path.join("volatile", "artifacts"));
    expect(String(webServer?.env?.PLAYWRIGHT_BROWSERS_PATH)).toContain(path.join("volatile", "cache", "playwright"));
  });

  it("does not inherit installed-app dist or bundle env by default", async () => {
    restoreEnv();
    process.env.CTX_WEB_DIST = "/Applications/ctx.app/Contents/Resources/web";
    process.env.CTX_BUNDLE_DIR = "/Applications/ctx.app/Contents/Resources/bundles";

    const config = await createCtxPlaywrightConfig("all");
    const webServer = Array.isArray(config.webServer) ? config.webServer[0] : config.webServer;

    expect(webServer?.env?.CTX_WEB_DIST).toBe("");
    expect(webServer?.env?.CTX_BUNDLE_DIR).not.toBe("/Applications/ctx.app/Contents/Resources/bundles");
  });

  it("allows a configured bundle dir only with an explicit e2e opt-in", async () => {
    restoreEnv();
    process.env.CTX_BUNDLE_DIR = "/tmp/custom-bundles";

    const withoutOptIn = await createCtxPlaywrightConfig("all");
    const withoutOptInWebServer = Array.isArray(withoutOptIn.webServer)
      ? withoutOptIn.webServer[0]
      : withoutOptIn.webServer;
    expect(withoutOptInWebServer?.env?.CTX_BUNDLE_DIR).not.toBe("/tmp/custom-bundles");

    restoreEnv();
    process.env.CTX_BUNDLE_DIR = "/tmp/custom-bundles";
    process.env.CTX_E2E_ALLOW_CONFIGURED_BUNDLE_DIR = "1";

    const withOptIn = await createCtxPlaywrightConfig("all");
    const withOptInWebServer = Array.isArray(withOptIn.webServer) ? withOptIn.webServer[0] : withOptIn.webServer;
    expect(withOptInWebServer?.env?.CTX_BUNDLE_DIR).toBe("/tmp/custom-bundles");
  });

  it("uses the checked-in bundle manifest when available", async () => {
    restoreEnv();
    const config = await createCtxPlaywrightConfig("all");
    const webServer = Array.isArray(config.webServer) ? config.webServer[0] : config.webServer;
    const expectedBundleDir = path.resolve(__dirname, "../desktop/src-tauri/bundles");
    const bundleManifestPath = path.join(expectedBundleDir, "manifest.json");

    if (fs.existsSync(bundleManifestPath)) {
      expect(webServer?.env?.CTX_BUNDLE_DIR).toBe(expectedBundleDir);
      expect(webServer?.env?.CTX_BUNDLE_MANIFEST).toBe(bundleManifestPath);
    } else {
      expect(webServer?.env?.CTX_BUNDLE_DIR).toBe("");
      expect(webServer?.env?.CTX_BUNDLE_MANIFEST).toBeUndefined();
    }
  });

  it("does not auto-select checked-in placeholder bundles for Bazel runfiles", async () => {
    restoreEnv();
    process.env.CTX_E2E_RUNTIME_SOURCE = "bazel-runfiles";

    const config = await createCtxPlaywrightConfig("premerge_required");
    const webServer = Array.isArray(config.webServer) ? config.webServer[0] : config.webServer;

    expect(webServer?.env?.CTX_BUNDLE_DIR).toBeUndefined();
    expect(webServer?.env?.CTX_BUNDLE_MANIFEST).toBeUndefined();
    expect(webServer?.env?.CTX_E2E_BUNDLED_ONLY).toBeUndefined();
  });

  it("uses CTX_E2E_BUNDLE_DIR instead of ambient CTX_BUNDLE_DIR", async () => {
    restoreEnv();
    process.env.CTX_BUNDLE_DIR = "/Applications/ctx.app/Contents/Resources/bundles";
    process.env.CTX_E2E_BUNDLE_DIR = "/tmp/ctx-e2e-bundles";

    const config = await createCtxPlaywrightConfig("premerge_required");
    const webServer = Array.isArray(config.webServer) ? config.webServer[0] : config.webServer;

    expect(webServer?.env?.CTX_BUNDLE_DIR).toBe("/tmp/ctx-e2e-bundles");
    expect(webServer?.env?.CTX_BUNDLE_MANIFEST).toBe("/tmp/ctx-e2e-bundles/manifest.json");
  });

  it("allows an explicit force-reuse override for required profiles", async () => {
    restoreEnv();
    process.env.CTX_E2E_REUSE_SERVER = "1";

    const withoutForce = await createCtxPlaywrightConfig("premerge_required");
    const withoutForceWebServer = Array.isArray(withoutForce.webServer)
      ? withoutForce.webServer[0]
      : withoutForce.webServer;
    expect(withoutForceWebServer?.reuseExistingServer).toBe(false);

    restoreEnv();
    process.env.CTX_E2E_FORCE_REUSE_SERVER = "1";

    const withForce = await createCtxPlaywrightConfig("premerge_required");
    const withForceWebServer = Array.isArray(withForce.webServer) ? withForce.webServer[0] : withForce.webServer;
    expect(withForceWebServer?.reuseExistingServer).toBe(true);
  });
});
