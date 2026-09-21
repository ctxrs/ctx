import assert from "node:assert/strict";
import {createHash} from "node:crypto";
import {gzipSync} from "node:zlib";
import test from "node:test";

import {decodePeekResponse, parseDlqArgs} from "../scripts/telemetry-queue-dlq.mjs";

const NOW = Date.parse("2026-09-01T16:00:00.000Z");

function peek(message, overrides = {}) {
  const compressed = gzipSync(Buffer.from(JSON.stringify(message)));
  return {
    success: true,
    result: {
      messages: [{
        id: "do-not-print-this-id",
        attempts: 10,
        body: compressed.toString("base64"),
        metadata: {"CF-Content-Type": "bytes"},
        timestamp_ms: NOW - 60_000,
        ...overrides,
      }],
    },
  };
}

function message(environment = "production") {
  return {
    format_version: 1,
    collision_visibility: {
      analytics_environment: environment,
      endpoint: "telemetry_batch",
      event_family: "operation_completed",
    },
    kind: "telemetry_row",
    row: {event_id: "sensitive-event-id"},
  };
}

function jsonWrapperPeek(value, overrides = {}) {
  const normalized = Buffer.from(JSON.stringify(value));
  const wrapper = JSON.stringify({
    body_base64: normalized.toString("base64"),
    body_sha256: createHash("sha256").update(normalized).digest("hex"),
    content_encoding: "identity",
    content_type: "application/json",
    format_version: 1,
    kind: "telemetry_queue_redrive",
  });
  return {
    success: true,
    result: {messages: [{
      attempts: 11,
      body: Buffer.from(wrapper, "utf8").toString("base64"),
      id: "do-not-print-this-id",
      metadata: {"CF-Content-Type": "json"},
      timestamp_ms: NOW - 60_000,
      ...overrides,
    }]},
  };
}

test("decodes a captured bytes message offline and emits only bounded metadata", () => {
  const receipt = decodePeekResponse(peek(message()), {environment: "prod", nowMs: NOW});

  assert.equal(receipt.schema_version, 2);
  assert.equal(receipt.message_count, 1);
  assert.equal(receipt.checked_at, "2026-09-01T16:00:00.000Z");
  assert.equal(receipt.source_queue, "ctx-telemetry-ingest-prod-dlq");
  assert.equal(receipt.messages[0].kind, "telemetry_row");
  assert.equal(receipt.messages[0].format_version, 1);
  assert.equal(receipt.messages[0].queue_content_type, "bytes");
  assert.equal(receipt.messages[0].age_seconds, 60);
  assert.match(receipt.messages[0].body_sha256, /^[0-9a-f]{64}$/u);
  assert(!JSON.stringify(receipt).includes("do-not-print-this-id"));
  assert(!JSON.stringify(receipt).includes("sensitive-event-id"));
});

test("decodes a retained JSON redrive wrapper without exposing its nested body", () => {
  const capture = jsonWrapperPeek(message());
  const receipt = decodePeekResponse(capture, {
    environment: "prod",
    nowMs: NOW,
  });

  assert.equal(receipt.messages[0].queue_content_type, "json");
  assert.equal(receipt.messages[0].kind, "telemetry_row");
  assert.equal(
    receipt.messages[0].body_sha256,
    createHash("sha256").update(Buffer.from(capture.result.messages[0].body, "base64")).digest("hex"),
  );
  assert.match(receipt.messages[0].body_sha256, /^[0-9a-f]{64}$/u);
  assert(!JSON.stringify(receipt).includes("sensitive-event-id"));
});

test("rejects environment mismatches and unsupported captures", () => {
  assert.throws(
    () => decodePeekResponse(peek(message("staging")), {environment: "prod", nowMs: NOW}),
    /analytics environment mismatch/u,
  );
  assert.throws(
    () => decodePeekResponse(peek(message(), {metadata: {"CF-Content-Type": "text"}}), {
      environment: "prod",
      nowMs: NOW,
    }),
    /content type is unsupported/u,
  );
});

test("rejects unknown format versions and malformed base64", () => {
  assert.throws(
    () => decodePeekResponse(peek({...message(), format_version: 2}), {environment: "prod", nowMs: NOW}),
    /unsupported queue format version/u,
  );
  assert.throws(
    () => decodePeekResponse(peek(message(), {body: "not-base64"}), {environment: "prod", nowMs: NOW}),
    /base64/u,
  );
  assert.throws(
    () => decodePeekResponse(jsonWrapperPeek(message(), {body: "not-base64"}), {
      environment: "prod",
      nowMs: NOW,
    }),
    /base64/u,
  );
});

test("rejects malformed retained JSON wrapper bytes before parsing", () => {
  assert.throws(
    () => decodePeekResponse(jsonWrapperPeek(message(), {
      body: Buffer.from([0xc3, 0x28]).toString("base64"),
    }), {environment: "prod", nowMs: NOW}),
    /JSON wrapper is not UTF-8/u,
  );
});

test("rejects retained JSON bodies above their encoded or normalized limits", () => {
  assert.throws(
    () => decodePeekResponse(jsonWrapperPeek(message(), {
      body: Buffer.alloc(127_801).toString("base64"),
    }), {environment: "prod", nowMs: NOW}),
    /size limit/u,
  );

  const oversizedNormalized = Buffer.alloc(64 * 1024 + 1);
  const wrapper = JSON.stringify({
    body_base64: oversizedNormalized.toString("base64"),
    body_sha256: createHash("sha256").update(oversizedNormalized).digest("hex"),
    content_encoding: "identity",
    content_type: "application/json",
    format_version: 1,
    kind: "telemetry_queue_redrive",
  });
  assert.throws(
    () => decodePeekResponse(jsonWrapperPeek(message(), {
      body: Buffer.from(wrapper, "utf8").toString("base64"),
    }), {environment: "prod", nowMs: NOW}),
    /size limit/u,
  );
});

test("rejects a retained JSON wrapper whose decoded body hash does not match", () => {
  const normalized = Buffer.from(JSON.stringify(message()));
  const wrapper = JSON.stringify({
    body_base64: normalized.toString("base64"),
    body_sha256: "0".repeat(64),
    content_encoding: "identity",
    content_type: "application/json",
    format_version: 1,
    kind: "telemetry_queue_redrive",
  });
  assert.throws(
    () => decodePeekResponse(jsonWrapperPeek(message(), {
      body: Buffer.from(wrapper, "utf8").toString("base64"),
    }), {environment: "prod", nowMs: NOW}),
    /redrive wrapper is invalid/u,
  );
});

test("requires an explicit input and environment", () => {
  assert.deepEqual(parseDlqArgs(["--input", "capture.json", "--environment", "staging"]), {
    input: "capture.json",
    environment: "staging",
    help: false,
  });
  assert.throws(() => parseDlqArgs(["--input", "capture.json"]), /environment/u);
});
