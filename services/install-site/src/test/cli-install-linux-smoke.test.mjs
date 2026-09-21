import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

test("Linux installation acceptance harness regressions (not native installation proof)", () => {
  const script = fileURLToPath(new URL("../../tests/install_linux_smoke_test.py", import.meta.url));
  const result = spawnSync("python3", ["-B", script], { encoding: "utf8", timeout: 30_000 });
  assert.ifError(result.error);
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
});
