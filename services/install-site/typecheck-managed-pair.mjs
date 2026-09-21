// Bazel supplies the existing release_api_npm integrity-locked TypeScript package
// and its pinned Node runtime. No ambient tsc or package installation fallback.
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const compiler = fileURLToPath(new URL(
  "../release-api-worker/node_modules/typescript/bin/tsc", import.meta.url,
));
const project = fileURLToPath(new URL("./tsconfig.managed-pair.json", import.meta.url));
const result = spawnSync(process.execPath, [compiler, "--project", project], {
  stdio: "inherit",
});
if (result.error) throw result.error;
process.exit(result.status ?? 1);
