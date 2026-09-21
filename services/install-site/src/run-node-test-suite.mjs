import process from "node:process";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const files = process.argv.slice(2);
const nodeBinary = process.env.JS_BINARY__NODE_BINARY ?? process.execPath;
if (files.length === 0) throw new Error("expected at least one node:test file");
const scratch = mkdtempSync(path.join(process.env.TEST_TMPDIR ?? tmpdir(), "ctx-installer-tests-"));
try {
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) =>
    key.startsWith("JS_BINARY__") || key.startsWith("RUNFILES_") ||
    ["PATH", "SystemRoot", "WINDIR", "ComSpec", "PATHEXT", "TEST_SRCDIR", "CTX_REQUIRE_POWERSHELL"].includes(key)));
  for (const key of ["HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "CTX_DATA_ROOT", "XDG_CONFIG_HOME",
    "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR", "CODEX_HOME", "CLAUDE_CONFIG_DIR", "COPILOT_HOME", "TMPDIR", "TEMP", "TMP"]) {
    env[key] = path.join(scratch, key.toLowerCase()); mkdirSync(env[key], { mode: 0o700 });
  }
  Object.assign(env, { CTX_ANALYTICS_ENABLED: "false", CTX_DAEMON_ENABLED: "false", CTX_UPGRADE_AUTO: "off", TEST_TMPDIR: scratch });
  const result = spawnSync(nodeBinary, ["--test", "--test-concurrency=1", ...files], { env, stdio: "inherit" });
  if (result.error) throw result.error;
  process.exitCode = result.status ?? 1;
} finally { rmSync(scratch, { recursive: true, force: true }); }
