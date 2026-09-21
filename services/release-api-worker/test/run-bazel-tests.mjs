import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const serviceRoot = fileURLToPath(new URL("../", import.meta.url));
const node = process.env.JS_BINARY__NODE_BINARY ?? process.execPath;
const mode = process.argv[2];
let args;
if (mode === "worker") {
  args = [join(dirname(require.resolve("vitest/package.json")), "vitest.mjs"), "run", "--maxWorkers=1", "--no-file-parallelism", "--configLoader=runner"];
} else if (mode === "typecheck") {
  args = [require.resolve("typescript/bin/tsc"), "--noEmit"];
} else {
  throw new Error(`unknown test mode: ${mode}`);
}
if (!process.env.TEST_TMPDIR) throw new Error("Bazel TEST_TMPDIR is required");
const scratch = mkdtempSync(join(process.env.TEST_TMPDIR, "release-api-"));
try {
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) =>
    key.startsWith("JS_BINARY__") || key.startsWith("RUNFILES_") || key === "TEST_SRCDIR"
  ));
  for (const key of ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR", "CTX_DATA_ROOT", "TMPDIR"]) {
    env[key] = join(scratch, key.toLowerCase());
    mkdirSync(env[key], { mode: 0o700 });
  }
  Object.assign(env, { PATH: dirname(node), TEST_TMPDIR: scratch, CI: "1", TZ: "UTC", LANG: "C.UTF-8" });
  const result = spawnSync(node, args, { cwd: serviceRoot, env, stdio: "inherit" });
  if (result.error) throw result.error;
  process.exitCode = result.status ?? 1;
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
