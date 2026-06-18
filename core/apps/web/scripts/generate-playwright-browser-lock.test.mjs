import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";

import {
  defaultHostPlatforms,
  defaultOutputPath,
  formatLockBzl,
  parseArgs,
} from "./generate-playwright-browser-lock.mjs";

test("generate-playwright-browser-lock uses the repo default output path and supported host platforms", () => {
  assert.deepEqual(parseArgs([]), {
    hostPlatforms: [...defaultHostPlatforms],
    outputPath: defaultOutputPath,
  });
});

test("generate-playwright-browser-lock parses repeated host platforms and explicit output path", () => {
  assert.deepEqual(parseArgs([
    "--host-platform",
    "mac15-arm64",
    "--host-platform",
    "ubuntu24.04-x64",
    "--output",
    "tmp/playwright-lock.bzl",
  ]), {
    hostPlatforms: ["mac15-arm64", "ubuntu24.04-x64"],
    outputPath: path.resolve("tmp/playwright-lock.bzl"),
  });
});

test("generate-playwright-browser-lock rejects missing option values", () => {
  assert.throws(() => parseArgs(["--host-platform"]), /missing value for --host-platform/u);
  assert.throws(() => parseArgs(["--output"]), /missing value for --output/u);
  assert.throws(
    () => parseArgs(["--output", "--host-platform", "mac15-arm64"]),
    /missing value for --output/u,
  );
});

test("generate-playwright-browser-lock formats a stable generated bzl lock file", () => {
  const rendered = formatLockBzl({
    generatedAt: "2026-04-24T16:00:00.000Z",
    playwrightVersion: "1.55.0",
    entries: {
      "mac15-arm64": {
        chromium: {
          directoryName: "chromium-1200",
          sha256: "abc123",
          url: "https://example.invalid/chromium.zip",
        },
        "chromium-headless-shell": {
          directoryName: "chromium_headless_shell-1200",
          sha256: "def456",
          url: "https://example.invalid/chromium-headless-shell.zip",
        },
      },
    },
  });

  assert.match(rendered, /PLAYWRIGHT_BROWSER_RUNTIME_LOCK = \{/u);
  assert.match(rendered, /"mac15-arm64"/u);
  assert.match(rendered, /"chromium"/u);
  assert.match(rendered, /"directory_name": "chromium-1200"/u);
  assert.match(rendered, /"chromium-headless-shell"/u);
  assert.match(rendered, /"directory_name": "chromium_headless_shell-1200"/u);
  assert.match(rendered, /"sha256": "abc123"/u);
});
