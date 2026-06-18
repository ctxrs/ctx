#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const supportedRuntimeProfiles = new Set(["workbench-lite", "agent-full", "web-artifact"]);
const suiteConfig = {
  premerge_required: "playwright.premerge.config.ts",
  quarantine: "playwright.premerge.config.ts",
  release_required: "playwright.release.config.ts",
  cross_platform: "playwright.cross-platform.config.ts",
  visual: "playwright.visual.config.ts",
  soak: "playwright.soak.config.ts",
  load: "playwright.load.config.ts",
};

const usage = () => [
  "usage: node apps/web/scripts/run-e2e-bazel-runtime.mjs \\",
  "  --config <playwright.config.ts> \\",
  "  --runtime-profile <workbench-lite|agent-full|web-artifact> \\",
  "  --ctx-http-bin <path> \\",
  "  [--web-dist <path>] \\",
  "  (--suite <suite>|--spec <e2e/spec.ts>) [--ctx-mcp-bin <path>] \\",
  "  [--playwright-browsers-dir <path>|--playwright-runtime-manifest <path>] [-- <playwright args...>]",
].join("\n");

export const normalizeSpec = (value) => {
  const raw = String(value || "").trim();
  if (!raw) {
    throw new Error("empty E2E spec path");
  }
  const prefixed = raw.startsWith("e2e/") ? raw : `e2e/${raw}`;
  const normalized = path.posix.normalize(prefixed.replace(/\\/gu, "/"));
  if (!normalized.startsWith("e2e/") || !normalized.endsWith(".spec.ts")) {
    throw new Error(`invalid E2E spec path: ${value}`);
  }
  return normalized;
};

export const normalizeSuiteManifestLine = (line) => {
  const raw = String(line || "").split("#", 1)[0].trim();
  return raw ? normalizeSpec(raw) : null;
};

export const parseArgs = (argv) => {
  const parsed = {
    config: "",
    ctxHttpBin: "",
    ctxMcpBin: "",
    forwardedArgs: [],
    playwrightBrowsersDir: "",
    playwrightRuntimeManifest: "",
    runtimeProfile: "",
    specs: [],
    suite: "",
    webDist: "",
  };
  const args = [...argv];
  while (args.length > 0) {
    const arg = args.shift();
    if (arg === "--") {
      parsed.forwardedArgs = args;
      break;
    }
    switch (arg) {
      case "--config":
        parsed.config = String(args.shift() || "").trim();
        break;
      case "--ctx-http-bin":
        parsed.ctxHttpBin = String(args.shift() || "").trim();
        break;
      case "--ctx-mcp-bin":
        parsed.ctxMcpBin = String(args.shift() || "").trim();
        break;
      case "--playwright-browsers-dir":
        parsed.playwrightBrowsersDir = String(args.shift() || "").trim();
        break;
      case "--playwright-runtime-manifest":
        parsed.playwrightRuntimeManifest = String(args.shift() || "").trim();
        break;
      case "--runtime-profile":
        parsed.runtimeProfile = String(args.shift() || "").trim();
        break;
      case "--web-dist":
        parsed.webDist = String(args.shift() || "").trim();
        break;
      case "--spec":
        parsed.specs.push(normalizeSpec(args.shift()));
        break;
      case "--suite":
        parsed.suite = String(args.shift() || "").trim();
        break;
      default:
        throw new Error(`unsupported argument: ${arg}\n${usage()}`);
    }
  }
  if (!parsed.config && parsed.suite && suiteConfig[parsed.suite]) {
    parsed.config = suiteConfig[parsed.suite];
  }
  if (!parsed.config) {
    throw new Error(`missing --config\n${usage()}`);
  }
  if (!supportedRuntimeProfiles.has(parsed.runtimeProfile)) {
    throw new Error(`unsupported --runtime-profile: ${parsed.runtimeProfile}`);
  }
  if (!parsed.ctxHttpBin) {
    throw new Error("missing --ctx-http-bin");
  }
  if (parsed.playwrightBrowsersDir && parsed.playwrightRuntimeManifest) {
    throw new Error("use --playwright-browsers-dir or --playwright-runtime-manifest, not both");
  }
  if (parsed.runtimeProfile === "agent-full" && !parsed.ctxMcpBin) {
    throw new Error("agent-full web E2E runtime requires --ctx-mcp-bin");
  }
  if (parsed.runtimeProfile === "web-artifact" && !parsed.webDist) {
    throw new Error("web-artifact web E2E runtime requires --web-dist");
  }
  if (!parsed.suite && parsed.specs.length === 0) {
    throw new Error("missing --suite or --spec");
  }
  return parsed;
};

const isRepoRoot = (candidate) =>
  fs.existsSync(path.join(candidate, "core", "package.json"))
  && fs.existsSync(path.join(candidate, "core", "apps", "web", "package.json"));

const normalizeRepoRootCandidate = (candidate) => {
  if (!candidate) return "";
  const resolved = path.resolve(candidate);
  if (isRepoRoot(resolved)) return resolved;
  if (path.basename(resolved) === "core" && isRepoRoot(path.dirname(resolved))) {
    return path.dirname(resolved);
  }
  return "";
};

const findRepoRootFrom = (startDir) => {
  let current = path.resolve(startDir);
  while (true) {
    const normalized = normalizeRepoRootCandidate(current);
    if (normalized) return normalized;
    const parent = path.dirname(current);
    if (parent === current) return "";
    current = parent;
  }
};

export const resolveRepoRoot = (env = process.env, cwd = process.cwd()) => {
  const moduleCandidate = path.resolve(__dirname, "../../../..");
  const moduleCandidateIsRunfiles = moduleCandidate.includes(".runfiles");
  const runfileCandidates = [
    env.TEST_SRCDIR && env.TEST_WORKSPACE ? path.join(env.TEST_SRCDIR, env.TEST_WORKSPACE) : "",
    env.RUNFILES_DIR && env.TEST_WORKSPACE ? path.join(env.RUNFILES_DIR, env.TEST_WORKSPACE) : "",
    env.RUNFILES_DIR ? path.join(env.RUNFILES_DIR, "_main") : "",
  ];
  const checkoutCandidates = [
    env.BUILD_WORKSPACE_DIRECTORY,
    env.CTX_REAL_WORKSPACE_ROOT,
    env.INIT_CWD,
    cwd,
    moduleCandidate,
  ];
  const hasBazelRunfiles = Boolean(env.TEST_SRCDIR || env.RUNFILES_DIR);
  let candidates = [...checkoutCandidates, ...runfileCandidates];
  if (hasBazelRunfiles) {
    candidates = [...runfileCandidates, moduleCandidate, ...checkoutCandidates];
  } else if (moduleCandidateIsRunfiles) {
    candidates = [moduleCandidate, ...checkoutCandidates, ...runfileCandidates];
  }
  for (const candidate of candidates) {
    const normalized = normalizeRepoRootCandidate(candidate);
    if (normalized) return normalized;
  }
  for (const candidate of candidates.filter(Boolean)) {
    const found = findRepoRootFrom(candidate);
    if (found) return found;
  }
  throw new Error("failed to locate ctx repo root for web E2E runtime");
};

const binName = (tool) => (process.platform === "win32" ? `${tool}.cmd` : tool);

const readJsonFile = (file) => JSON.parse(fs.readFileSync(file, "utf8"));

const packagePathParts = (packageName) => packageName.split("/").filter(Boolean);

const packageBinPath = (pkg, packageName, tool) => {
  const bin = pkg?.bin;
  if (typeof bin === "string") return bin;
  if (!bin || typeof bin !== "object" || Array.isArray(bin)) return "";
  const packageBaseName = packagePathParts(packageName).at(-1) || packageName;
  const candidate = bin[tool] ?? bin[packageBaseName];
  return typeof candidate === "string" ? candidate : "";
};

const resolveNodePackageBin = (packageRoot, packageName, tool) => {
  let current = path.resolve(packageRoot);
  while (true) {
    const packageDir = path.join(current, "node_modules", ...packagePathParts(packageName));
    const packageJsonPath = path.join(packageDir, "package.json");
    if (fs.existsSync(packageJsonPath)) {
      const binPath = packageBinPath(readJsonFile(packageJsonPath), packageName, tool);
      if (binPath) {
        const candidate = path.join(packageDir, binPath);
        if (fs.existsSync(candidate)) return candidate;
      }
    }
    const parent = path.dirname(current);
    if (parent === current) break;
    current = parent;
  }
  return "";
};

export const resolveLocalNodeBin = (packageRoot, tool) => {
  const expected = binName(tool);
  let current = path.resolve(packageRoot);
  while (true) {
    const candidate = path.join(current, "node_modules", ".bin", expected);
    if (fs.existsSync(candidate)) return candidate;
    const parent = path.dirname(current);
    if (parent === current) break;
    current = parent;
  }
  const packageBin = resolveNodePackageBin(packageRoot, tool, tool);
  if (packageBin) return packageBin;
  const expectedPath = path.join(path.resolve(packageRoot), "node_modules", ".bin", expected);
  throw new Error(
    `Missing local ${tool} binary at ${expectedPath}. Run pnpm install in core before running Bazel web E2E.`,
  );
};

export const resolveExistingPath = (configured, { cwd = process.cwd(), env = process.env, repoRoot } = {}) => {
  const raw = String(configured || "").trim();
  if (!raw) {
    throw new Error("missing path");
  }
  const candidates = [];
  if (path.isAbsolute(raw)) {
    candidates.push(raw);
  } else {
    candidates.push(
      path.resolve(cwd, raw),
      repoRoot ? path.resolve(repoRoot, raw) : "",
      env.TEST_SRCDIR && env.TEST_WORKSPACE ? path.join(env.TEST_SRCDIR, env.TEST_WORKSPACE, raw) : "",
      env.RUNFILES_DIR && env.TEST_WORKSPACE ? path.join(env.RUNFILES_DIR, env.TEST_WORKSPACE, raw) : "",
      env.RUNFILES_DIR ? path.join(env.RUNFILES_DIR, "_main", raw) : "",
    );
  }
  for (const candidate of candidates.filter(Boolean)) {
    if (fs.existsSync(candidate)) return path.resolve(candidate);
  }
  throw new Error(`declared Bazel runtime input does not exist: ${raw}`);
};

const readSuiteSpecs = (webRoot, suite) => {
  if (!suiteConfig[suite]) {
    throw new Error(`unsupported E2E suite: ${suite}`);
  }
  const manifestPath = path.join(webRoot, "e2e", "suites", `${suite}.txt`);
  const specs = fs
    .readFileSync(manifestPath, "utf8")
    .split(/\r?\n/u)
    .map(normalizeSuiteManifestLine)
    .filter(Boolean);
  if (specs.length === 0) {
    throw new Error(`E2E suite is empty: ${suite}`);
  }
  return [...new Set(specs)].sort();
};

const resolveSpecs = (webRoot, { suite, specs }) => {
  const resolved = [
    ...(suite ? readSuiteSpecs(webRoot, suite) : []),
    ...specs,
  ];
  const deduped = [...new Set(resolved)].sort();
  for (const spec of deduped) {
    const abs = path.join(webRoot, spec);
    if (!fs.existsSync(abs)) {
      throw new Error(`E2E spec does not exist: ${spec}`);
    }
  }
  return deduped;
};

const ensureTempRoot = (env) => {
  const root = path.resolve(env.TEST_TMPDIR || env.CTX_E2E_TMPDIR || os.tmpdir());
  fs.mkdirSync(root, { recursive: true });
  return root;
};

const materializedTreeExcludes = new Set([
  ".git",
  "dist",
  "node_modules",
  "playwright-report",
  "test-results",
]);

const isBazelRunfilesPath = (candidate) => path.resolve(candidate).includes(".runfiles");

const treeCopyFilter = (sourceRoot) => (sourcePath) => {
  const relative = path.relative(sourceRoot, sourcePath);
  if (!relative) return true;
  const parts = relative.split(path.sep);
  return !parts.some((part) => materializedTreeExcludes.has(part));
};

const copyTreeIfExists = (sourcePath, targetPath) => {
  if (!fs.existsSync(sourcePath)) return;
  fs.cpSync(sourcePath, targetPath, {
    dereference: true,
    filter: treeCopyFilter(sourcePath),
    force: true,
    recursive: true,
  });
};

const copyFileIfExists = (sourcePath, targetPath) => {
  if (!fs.existsSync(sourcePath)) return;
  fs.mkdirSync(path.dirname(targetPath), { recursive: true });
  fs.copyFileSync(sourcePath, targetPath);
};

const symlinkDirectoryIfExists = (sourcePath, targetPath) => {
  if (!fs.existsSync(sourcePath)) return;
  fs.mkdirSync(path.dirname(targetPath), { recursive: true });
  fs.symlinkSync(
    path.resolve(sourcePath),
    targetPath,
    process.platform === "win32" ? "junction" : "dir",
  );
};

export const materializeBazelWebRepo = ({ repoRoot, runtimeProfile, tempRoot }) => {
  const sourceCoreRoot = path.join(repoRoot, "core");
  const materializedRepoRoot = path.join(
    tempRoot,
    `ctx-web-e2e-runfiles-repo-${runtimeProfile}-${process.pid}`,
  );
  const materializedCoreRoot = path.join(materializedRepoRoot, "core");
  fs.rmSync(materializedRepoRoot, { recursive: true, force: true });
  fs.mkdirSync(materializedCoreRoot, { recursive: true });

  for (const fileName of ["package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml"]) {
    copyFileIfExists(
      path.join(sourceCoreRoot, fileName),
      path.join(materializedCoreRoot, fileName),
    );
  }

  copyTreeIfExists(
    path.join(sourceCoreRoot, "apps", "web"),
    path.join(materializedCoreRoot, "apps", "web"),
  );
  copyTreeIfExists(
    path.join(sourceCoreRoot, "apps", "desktop", "src-tauri", "bundles"),
    path.join(materializedCoreRoot, "apps", "desktop", "src-tauri", "bundles"),
  );
  copyTreeIfExists(
    path.join(sourceCoreRoot, "packages"),
    path.join(materializedCoreRoot, "packages"),
  );
  copyTreeIfExists(
    path.join(sourceCoreRoot, "scripts"),
    path.join(materializedCoreRoot, "scripts"),
  );

  symlinkDirectoryIfExists(
    path.join(sourceCoreRoot, "node_modules"),
    path.join(materializedCoreRoot, "node_modules"),
  );
  symlinkDirectoryIfExists(
    path.join(sourceCoreRoot, "apps", "web", "node_modules"),
    path.join(materializedCoreRoot, "apps", "web", "node_modules"),
  );

  return materializedRepoRoot;
};

export const prepareRuntimeRepoRoot = ({ env = process.env, repoRoot, runtimeProfile, tempRoot }) => {
  if (!isBazelRunfilesPath(repoRoot) && !env.TEST_SRCDIR && !env.RUNFILES_DIR) {
    return repoRoot;
  }
  return materializeBazelWebRepo({ repoRoot, runtimeProfile, tempRoot });
};

export const envWithCurrentNodeOnPath = (env = process.env) => {
  const nodeDir = path.dirname(process.execPath);
  const currentPath = String(env.PATH || "");
  const entries = currentPath.split(path.delimiter).filter(Boolean);
  return {
    ...env,
    PATH: entries.includes(nodeDir)
      ? currentPath
      : [nodeDir, ...entries].join(path.delimiter),
  };
};

const run = (command, args, options) => {
  const result = spawnSync(command, args, { ...options, stdio: "inherit" });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
};

export const pathsReferToSameFile = (left, right, realpathSync = fs.realpathSync) => {
  const rawLeft = String(left || "").trim();
  const rawRight = String(right || "").trim();
  if (!rawLeft || !rawRight) return false;
  const resolvedLeft = path.resolve(rawLeft);
  const resolvedRight = path.resolve(rawRight);
  if (resolvedLeft === resolvedRight) return true;
  try {
    return realpathSync(resolvedLeft) === realpathSync(resolvedRight);
  } catch {
    return false;
  }
};

const buildWebDist = ({ env, runtimeProfile, tempRoot, viteBin, webRoot }) => {
  const distDir = path.join(tempRoot, `ctx-web-e2e-dist-${runtimeProfile}-${process.pid}`);
  fs.rmSync(distDir, { recursive: true, force: true });
  run(viteBin, ["build", "--outDir", distDir, "--emptyOutDir"], {
    cwd: webRoot,
    env,
  });
  return distDir;
};

export const resolveWebDistDir = ({
  buildEnv,
  env,
  runtimeProfile,
  sourceRepoRoot,
  tempRoot,
  viteBin,
  webDist,
  webRoot,
}) => {
  if (runtimeProfile === "web-artifact") {
    return resolveExistingPath(webDist, { env, repoRoot: sourceRepoRoot });
  }
  return buildWebDist({
    env: buildEnv,
    runtimeProfile,
    tempRoot,
    viteBin,
    webRoot,
  });
};

export const resolveViteBinForRuntimeProfile = (webRoot, runtimeProfile) => (
  runtimeProfile === "web-artifact" ? "" : resolveLocalNodeBin(webRoot, "vite")
);

const playwrightHostPlatform = ({ platform = process.platform, arch = process.arch } = {}) => {
  if (platform === "darwin" && arch === "arm64") return "mac15-arm64";
  if (platform === "linux" && arch === "x64") return "ubuntu24.04-x64";
  throw new Error(`unsupported Bazel Playwright browser host platform: ${platform}/${arch}`);
};

export const resolvePlaywrightBrowsersPath = (
  configured,
  { platform = process.platform, arch = process.arch } = {},
) => {
  const raw = String(configured || "").trim();
  if (!raw) {
    throw new Error("missing Playwright browser runtime path");
  }
  const root = path.resolve(raw);
  if (!fs.existsSync(root)) {
    throw new Error(`declared Playwright browser runtime does not exist: ${configured}`);
  }
  const hostRoot = path.join(root, playwrightHostPlatform({ platform, arch }));
  if (fs.existsSync(hostRoot)) return hostRoot;
  return root;
};

export const resolvePlaywrightBrowsersPathFromManifest = (
  configured,
  { platform = process.platform, arch = process.arch } = {},
) => {
  const raw = String(configured || "").trim();
  if (!raw) {
    throw new Error("missing Playwright browser runtime manifest path");
  }
  const manifestPath = path.resolve(raw);
  if (!fs.existsSync(manifestPath)) {
    throw new Error(`declared Playwright browser runtime manifest does not exist: ${configured}`);
  }
  const manifest = readJsonFile(manifestPath);
  const hostPlatform = playwrightHostPlatform({ platform, arch });
  const platformEntries = manifest?.platforms?.[hostPlatform];
  if (!platformEntries || typeof platformEntries !== "object" || Array.isArray(platformEntries)) {
    throw new Error(`Playwright runtime manifest does not include host platform: ${hostPlatform}`);
  }
  const manifestDir = path.dirname(manifestPath);
  let hostRoot = "";
  for (const [browserName, entry] of Object.entries(platformEntries)) {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) {
      throw new Error(`invalid Playwright runtime manifest entry for ${browserName} on ${hostPlatform}`);
    }
    const directory = String(entry.directory || "").trim();
    const runtimePath = String(entry.path || "").trim();
    if (!directory || !runtimePath) {
      throw new Error(`invalid Playwright runtime manifest entry for ${browserName} on ${hostPlatform}`);
    }
    const browserRoot = path.resolve(manifestDir, runtimePath);
    if (!fs.existsSync(browserRoot) || !fs.statSync(browserRoot).isDirectory()) {
      throw new Error(`missing Playwright runtime tree for ${browserName} on ${hostPlatform}: ${browserRoot}`);
    }
    if (path.basename(browserRoot) !== directory) {
      throw new Error(`Playwright runtime manifest directory mismatch for ${browserName} on ${hostPlatform}`);
    }
    const candidateHostRoot = path.dirname(browserRoot);
    if (!hostRoot) {
      hostRoot = candidateHostRoot;
    } else if (hostRoot !== candidateHostRoot) {
      throw new Error(`Playwright runtime manifest has inconsistent roots for ${hostPlatform}`);
    }
  }
  if (!hostRoot) {
    throw new Error(`Playwright runtime manifest has no browser entries for ${hostPlatform}`);
  }
  return hostRoot;
};

export const buildPlaywrightEnv = ({
  ctxHttpBin,
  ctxMcpBin = "",
  env = process.env,
  playwrightBrowsersPath = "",
  runtimeProfile,
  tempRoot,
  webDistDir,
}) => {
  const e2eTmpDir = path.join(tempRoot, `ctx-e2e-${runtimeProfile}-tmp-${process.pid}`);
  const e2eDataDir = path.join(e2eTmpDir, `ctx-e2e-${runtimeProfile}-data-${process.pid}`);
  const nextEnv = {
    ...envWithCurrentNodeOnPath(env),
    CI: env.CI ?? "1",
    CTX_E2E_CTX_HTTP_BIN: ctxHttpBin,
    CTX_E2E_DATA_DIR: e2eDataDir,
    CTX_E2E_RUNTIME_PROFILE: runtimeProfile,
    CTX_E2E_RUNTIME_SOURCE: "bazel-runfiles",
    CTX_E2E_SKIP_WEB_BUILD: "1",
    CTX_E2E_TMPDIR: e2eTmpDir,
    CTX_E2E_WEB_DIST: webDistDir,
    CTX_VOLATILE_TMPDIR: env.CTX_VOLATILE_TMPDIR || tempRoot,
    TMP: e2eTmpDir,
    TEMP: e2eTmpDir,
    TMPDIR: e2eTmpDir,
  };
  if (playwrightBrowsersPath) {
    nextEnv.PLAYWRIGHT_BROWSERS_PATH = playwrightBrowsersPath;
  } else if (env.PLAYWRIGHT_BROWSERS_PATH) {
    nextEnv.PLAYWRIGHT_BROWSERS_PATH = env.PLAYWRIGHT_BROWSERS_PATH;
  }
  delete nextEnv.CTX_MCP_DISABLED;
  if (runtimeProfile === "agent-full") {
    nextEnv.CTX_E2E_CTX_MCP_BIN = ctxMcpBin;
  } else {
    delete nextEnv.CTX_E2E_CTX_MCP_BIN;
    delete nextEnv.CTX_MCP_COMMAND;
  }
  return nextEnv;
};

export const buildPlaywrightArgs = ({ config, forwardedArgs = [], specs }) => [
  "test",
  "-c",
  config,
  ...specs,
  ...forwardedArgs,
];

export const runBazelRuntimeE2E = (argv, env = process.env) => {
  const options = parseArgs(argv);
  const sourceRepoRoot = resolveRepoRoot(env);
  const tempRoot = ensureTempRoot(env);
  const repoRoot = prepareRuntimeRepoRoot({
    env,
    repoRoot: sourceRepoRoot,
    runtimeProfile: options.runtimeProfile,
    tempRoot,
  });
  const coreRoot = path.join(repoRoot, "core");
  const webRoot = path.join(coreRoot, "apps", "web");
  const ctxHttpBin = resolveExistingPath(options.ctxHttpBin, { env, repoRoot: sourceRepoRoot });
  const ctxMcpBin = options.ctxMcpBin
    ? resolveExistingPath(options.ctxMcpBin, { env, repoRoot: sourceRepoRoot })
    : "";
  const playwrightBrowsersPath = options.playwrightBrowsersDir
    ? resolvePlaywrightBrowsersPath(
      resolveExistingPath(options.playwrightBrowsersDir, { env, repoRoot: sourceRepoRoot }),
    )
    : options.playwrightRuntimeManifest
      ? resolvePlaywrightBrowsersPathFromManifest(
        resolveExistingPath(options.playwrightRuntimeManifest, { env, repoRoot: sourceRepoRoot }),
      )
    : "";
  const specs = resolveSpecs(webRoot, options);
  const viteBin = resolveViteBinForRuntimeProfile(webRoot, options.runtimeProfile);
  const playwrightBin = resolveLocalNodeBin(webRoot, "playwright");
  const buildEnv = {
    ...envWithCurrentNodeOnPath(env),
    TMP: tempRoot,
    TEMP: tempRoot,
    TMPDIR: tempRoot,
  };
  const webDistDir = resolveWebDistDir({
    buildEnv,
    env,
    runtimeProfile: options.runtimeProfile,
    sourceRepoRoot,
    tempRoot,
    viteBin,
    webDist: options.webDist,
    webRoot,
  });
  const playwrightEnv = buildPlaywrightEnv({
    ctxHttpBin,
    ctxMcpBin,
    env,
    playwrightBrowsersPath,
    runtimeProfile: options.runtimeProfile,
    tempRoot,
    webDistDir,
  });
  const playwrightArgs = buildPlaywrightArgs({
    config: options.config,
    forwardedArgs: options.forwardedArgs,
    specs,
  });
  run(playwrightBin, playwrightArgs, {
    cwd: webRoot,
    env: playwrightEnv,
  });
};

if (process.argv[1] && pathsReferToSameFile(process.argv[1], __filename)) {
  try {
    runBazelRuntimeE2E(process.argv.slice(2));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
