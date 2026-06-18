#!/usr/bin/env node

import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";

const parseBool = (value) => ["1", "true", "yes", "on"].includes(String(value ?? "").trim().toLowerCase());

const requireEnv = (key, env = process.env) => {
  const value = String(env[key] ?? "").trim();
  if (!value) {
    throw new Error(`Missing required environment variable: ${key}`);
  }
  return value;
};

export const ensureSafeE2ETempDir = (dataDir, env = process.env) => {
  const resolved = path.resolve(dataDir);
  const configuredTmpRoot = String(env.CTX_VOLATILE_TMPDIR ?? "").trim();
  const tmpRoot = path.resolve(configuredTmpRoot || os.tmpdir());
  const relative = path.relative(tmpRoot, resolved);
  if (relative.startsWith("..") || path.isAbsolute(relative)) {
    throw new Error(`Refusing to delete non-temp e2e data dir: ${resolved}`);
  }
  if (!path.basename(resolved).startsWith("ctx-e2e-")) {
    throw new Error(`Refusing to delete unexpected e2e data dir name: ${resolved}`);
  }
  return resolved;
};

export const ensureE2ETempDir = (dataDir, env = process.env) => {
  const resolved = ensureSafeE2ETempDir(dataDir, env);
  fs.mkdirSync(resolved, { recursive: true });
  return resolved;
};

export const prepareE2EServerDirs = (dataDir, tmpDir = dataDir, env = process.env) => {
  const resolvedDataDir = ensureE2ETempDir(dataDir, env);
  const resolvedTmpDir = ensureE2ETempDir(tmpDir, env);
  fs.rmSync(resolvedDataDir, { recursive: true, force: true });
  fs.mkdirSync(resolvedDataDir, { recursive: true });
  if (resolvedTmpDir !== resolvedDataDir) {
    fs.mkdirSync(resolvedTmpDir, { recursive: true });
  }
  return {
    dataDir: resolvedDataDir,
    tmpDir: resolvedTmpDir,
  };
};

const runSync = (command, args, cwd, env) => {
  const result = spawnSync(command, args, { cwd, env, stdio: "inherit" });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
};

const resolveConfiguredPath = (configured, cwd) =>
  path.isAbsolute(configured) ? path.resolve(configured) : path.resolve(cwd, configured);

export const resolveCargoTargetDir = (coreRoot, env = process.env) => {
  const configured = String(env.CARGO_TARGET_DIR ?? "").trim();
  return configured ? resolveConfiguredPath(configured, coreRoot) : path.join(coreRoot, "target");
};

export const ensureCargoTargetDir = (coreRoot, env = process.env) => {
  const cargoTargetDir = resolveCargoTargetDir(coreRoot, env);
  fs.mkdirSync(cargoTargetDir, { recursive: true });
  return cargoTargetDir;
};

const packagePathParts = (packageName) => packageName.split("/").filter(Boolean);

const readJsonFile = (file) => JSON.parse(fs.readFileSync(file, "utf8"));

const packageBinPath = (pkg, packageName, tool) => {
  const bin = pkg?.bin;
  if (typeof bin === "string") return bin;
  if (!bin || typeof bin !== "object" || Array.isArray(bin)) return "";
  const packageBaseName = packagePathParts(packageName).at(-1) || packageName;
  return typeof (bin[tool] ?? bin[packageBaseName]) === "string"
    ? bin[tool] ?? bin[packageBaseName]
    : "";
};

const resolveNodePackageBin = (packageRoot, packageName, tool) => {
  let current = path.resolve(packageRoot);
  while (true) {
    const packageDir = path.join(current, "node_modules", ...packagePathParts(packageName));
    const packageJsonPath = path.join(packageDir, "package.json");
    if (fs.existsSync(packageJsonPath)) {
      const binPath = packageBinPath(readJsonFile(packageJsonPath), packageName, tool);
      const candidate = binPath ? path.join(packageDir, binPath) : "";
      if (candidate && fs.existsSync(candidate)) return candidate;
    }
    const parent = path.dirname(current);
    if (parent === current) break;
    current = parent;
  }
  return "";
};

export const resolveLocalNodeBin = (packageRoot, tool) => {
  const binName = process.platform === "win32" ? `${tool}.cmd` : tool;
  let current = path.resolve(packageRoot);
  while (true) {
    const candidate = path.join(current, "node_modules", ".bin", binName);
    if (fs.existsSync(candidate)) return candidate;
    const parent = path.dirname(current);
    if (parent === current) break;
    current = parent;
  }
  const packageBin = resolveNodePackageBin(packageRoot, tool, tool);
  if (packageBin) return packageBin;
  throw new Error(`Missing local ${tool} binary under ${path.resolve(packageRoot)}`);
};

export const resolveWebBuildArgs = (webDistDir) => [
  "build",
  "--outDir",
  webDistDir,
  "--emptyOutDir",
];

export const resolveServeWebDistDir = (coreRoot, env = process.env, skipWebBuild = false) => {
  const configuredE2E = String(env.CTX_E2E_WEB_DIST ?? "").trim();
  if (configuredE2E) return resolveConfiguredPath(configuredE2E, coreRoot);
  const configured = String(env.CTX_WEB_DIST ?? "").trim();
  if (configured && parseBool(env.CTX_E2E_ALLOW_CONFIGURED_WEB_DIST)) {
    return resolveConfiguredPath(configured, coreRoot);
  }
  if (skipWebBuild) {
    return path.join(coreRoot, "apps", "web", "dist");
  }
  return path.join(coreRoot, "apps", "web", "dist-e2e");
};

export const resolveE2ERuntimeSource = (env = process.env) => {
  const source = String(env.CTX_E2E_RUNTIME_SOURCE ?? "").trim();
  if (!source) return "local-build";
  if (source === "bazel-runfiles") return source;
  throw new Error(`Unsupported CTX_E2E_RUNTIME_SOURCE: ${source}`);
};

export const resolveE2ERuntimeProfile = (env = process.env) => {
  const profile = String(env.CTX_E2E_RUNTIME_PROFILE ?? "").trim() || "workbench-lite";
  if (["workbench-lite", "agent-full", "web-artifact"].includes(profile)) return profile;
  throw new Error(`Unsupported CTX_E2E_RUNTIME_PROFILE: ${profile}`);
};

const requireExecutableFile = (key, env = process.env) => {
  const resolved = path.resolve(requireEnv(key, env));
  if (!fs.existsSync(resolved) || !fs.statSync(resolved).isFile()) {
    throw new Error(`${key} does not point to a file: ${resolved}`);
  }
  return resolved;
};

const requireDirectory = (key, env = process.env) => {
  const resolved = path.resolve(requireEnv(key, env));
  if (!fs.existsSync(resolved) || !fs.statSync(resolved).isDirectory()) {
    throw new Error(`${key} does not point to a directory: ${resolved}`);
  }
  return resolved;
};

export const resolveBazelRunfilesRuntime = (env = process.env) => {
  const runtimeProfile = resolveE2ERuntimeProfile(env);
  const runtime = {
    ctxHttpBin: requireExecutableFile("CTX_E2E_CTX_HTTP_BIN", env),
    ctxMcpBin: "",
    runtimeProfile,
    webDistDir: requireDirectory("CTX_E2E_WEB_DIST", env),
  };
  if (runtimeProfile === "agent-full") {
    runtime.ctxMcpBin = requireExecutableFile("CTX_E2E_CTX_MCP_BIN", env);
  }
  return runtime;
};

const ensureCtxMcpCommand = (coreRoot, env) => {
  const configured = String(env.CTX_MCP_COMMAND ?? "").trim();
  if (configured && parseBool(env.CTX_E2E_ALLOW_CONFIGURED_MCP_COMMAND)) {
    return configured;
  }
  ensureCargoTargetDir(coreRoot, env);
  const cargoCmd = process.platform === "win32" ? "cargo.exe" : "cargo";
  runSync(cargoCmd, ["build", "-p", "ctx-mcp", "--bin", "ctx-mcp"], coreRoot, env);
  const binName = process.platform === "win32" ? "ctx-mcp.exe" : "ctx-mcp";
  const binaryPath = path.join(resolveCargoTargetDir(coreRoot, env), "debug", binName);
  if (!fs.existsSync(binaryPath)) {
    throw new Error(`ctx-mcp binary not found after build: ${binaryPath}`);
  }
  return binaryPath;
};

const buildWebDistIfNeeded = ({ bazelRuntime, coreRoot, env, skipWebBuild, webDistDir }) => {
  if (bazelRuntime || skipWebBuild) return;
  const webRoot = path.join(coreRoot, "apps", "web");
  const viteBin = resolveLocalNodeBin(webRoot, "vite");
  fs.rmSync(webDistDir, { recursive: true, force: true });
  runSync(viteBin, resolveWebBuildArgs(webDistDir), webRoot, env);
};

export const buildServerLaunch = ({
  coreRoot = process.cwd(),
  env = process.env,
  host = env.CTX_E2E_HOST ?? "127.0.0.1",
  port,
}) => {
  const runtimeSource = resolveE2ERuntimeSource(env);
  const bazelRuntime = runtimeSource === "bazel-runfiles" ? resolveBazelRunfilesRuntime(env) : null;
  const runtimeProfile = bazelRuntime?.runtimeProfile ?? resolveE2ERuntimeProfile(env);
  const nextEnv = { ...env };
  if (bazelRuntime) {
    if (runtimeProfile === "agent-full") {
      nextEnv.CTX_MCP_COMMAND = bazelRuntime.ctxMcpBin;
      delete nextEnv.CTX_MCP_DISABLED;
    } else {
      nextEnv.CTX_MCP_DISABLED = "1";
      delete nextEnv.CTX_MCP_COMMAND;
    }
  } else if (runtimeProfile === "agent-full") {
    nextEnv.CTX_MCP_COMMAND = ensureCtxMcpCommand(coreRoot, nextEnv);
  }
  const skipWebBuild = parseBool(nextEnv.CTX_E2E_SKIP_WEB_BUILD);
  const webDistDir = bazelRuntime?.webDistDir ?? resolveServeWebDistDir(coreRoot, nextEnv, skipWebBuild);
  buildWebDistIfNeeded({ bazelRuntime, coreRoot, env: nextEnv, skipWebBuild, webDistDir });

  const cargoCmd = process.platform === "win32" ? "cargo.exe" : "cargo";
  return {
    command: bazelRuntime?.ctxHttpBin ?? cargoCmd,
    args: bazelRuntime
      ? ["serve", "--bind", `${host}:${port}`, "--data-dir", nextEnv.CTX_E2E_DATA_DIR]
      : [
        "run",
        "-p",
        "ctx-http",
        "--bin",
        "ctx",
        "--",
        "serve",
        "--bind",
        `${host}:${port}`,
        "--data-dir",
        nextEnv.CTX_E2E_DATA_DIR,
      ],
    env: {
      ...nextEnv,
      CTX_EXECUTION_MODE: "host",
      CTX_SHOW_FAKE_PROVIDER: "1",
      CTX_STORAGE_BACKEND: "sqlite",
      CTX_WEB_DIST: webDistDir,
    },
  };
};

const main = () => {
  const coreRoot = process.cwd();
  const host = process.env.CTX_E2E_HOST ?? "127.0.0.1";
  const portText = requireEnv("CTX_E2E_PORT");
  const port = Number(portText);
  if (!Number.isInteger(port) || port <= 0 || port > 65535) {
    throw new Error(`Invalid CTX_E2E_PORT: ${portText}`);
  }

  const { dataDir, tmpDir } = prepareE2EServerDirs(
    requireEnv("CTX_E2E_DATA_DIR"),
    process.env.CTX_E2E_TMPDIR ?? requireEnv("CTX_E2E_DATA_DIR"),
  );
  const env = {
    ...process.env,
    CTX_E2E_DATA_DIR: dataDir,
    TMP: tmpDir,
    TEMP: tmpDir,
    TMPDIR: tmpDir,
  };
  fs.writeFileSync(path.join(dataDir, "daemon_auth.json"), JSON.stringify({
    token: requireEnv("CTX_E2E_AUTH_TOKEN", env),
  }, null, 2));
  fs.writeFileSync(path.join(dataDir, "settings.json"), JSON.stringify({
    execution: { mode: "host" },
  }, null, 2));

  const launch = buildServerLaunch({ coreRoot, env, host, port });
  const child = spawn(launch.command, launch.args, {
    cwd: coreRoot,
    env: launch.env,
    stdio: "inherit",
  });

  const relaySignal = (signal) => {
    if (!child.killed) {
      child.kill(signal);
    }
  };

  process.on("SIGINT", () => relaySignal("SIGINT"));
  process.on("SIGTERM", () => relaySignal("SIGTERM"));
  process.on("SIGHUP", () => relaySignal("SIGHUP"));

  child.on("error", (err) => {
    console.error("Failed to launch ctx e2e server:", err);
    process.exit(1);
  });

  child.on("exit", (code, signal) => {
    if (signal) {
      process.kill(process.pid, signal);
      return;
    }
    process.exit(code ?? 1);
  });
};

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    main();
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
