import process from "node:process";
import { spawnSync } from "node:child_process";

const files = process.argv.slice(2);
const nodeBinary = process.env.JS_BINARY__NODE_BINARY ?? process.execPath;

if (files.length === 0) {
  process.stderr.write("expected at least one node:test file\n");
  process.exit(2);
}

const result = spawnSync(nodeBinary, ["--test", ...files], {
  stdio: "inherit",
});

if (typeof result.status === "number") {
  process.exit(result.status);
}
if (result.error) {
  throw result.error;
}
process.exit(1);
