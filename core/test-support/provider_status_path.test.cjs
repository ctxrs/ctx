"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { normalizeTarget, providerStatusPath } = require("./provider_status_path.cjs");

test("normalizeTarget defaults to host", () => {
  assert.equal(normalizeTarget(undefined), "host");
  assert.equal(normalizeTarget(""), "host");
  assert.equal(normalizeTarget("weird"), "host");
});

test("normalizeTarget preserves container", () => {
  assert.equal(normalizeTarget("container"), "container");
});

test("providerStatusPath includes explicit target query", () => {
  assert.equal(providerStatusPath("codex", "host"), "/api/providers/codex?target=host");
  assert.equal(providerStatusPath("codex", "container"), "/api/providers/codex?target=container");
});

test("providerStatusPath encodes provider ids", () => {
  assert.equal(
    providerStatusPath("provider/with spaces", "container"),
    "/api/providers/provider%2Fwith%20spaces?target=container",
  );
});
