import test from "node:test";
import assert from "node:assert/strict";

import { isWorkerAuthValid } from "./auth.mjs";

test("isWorkerAuthValid accepts the expected header value", () => {
  assert.equal(
    isWorkerAuthValid({ "x-ctx-worker-auth": "secret" }, "secret"),
    true,
  );
});

test("isWorkerAuthValid rejects missing or mismatched headers", () => {
  assert.equal(isWorkerAuthValid({}, "secret"), false);
  assert.equal(
    isWorkerAuthValid({ "x-ctx-worker-auth": "wrong" }, "secret"),
    false,
  );
});
