import { readFileSync } from "node:fs";

import { expect, test } from "vitest";

import {
  BLAME_INSTALLATION_KEY_HEADER,
  BLAME_PROOF_EVENT_ID_HEADER,
  BLAME_PROOF_NONCE_HEADER,
  BLAME_PROOF_SIGNATURE_HEADER,
  BLAME_PROOF_TIME_HEADER,
} from "../src/blame-installation-proof";
import { ENV, workerHarness } from "./worker-test-fixtures";

const fixtureUrl = new URL(
  "./fixtures/blame-product-v1/legacy-rust-proof.json",
  import.meta.url,
);
const vector = JSON.parse(readFileSync(fixtureUrl, "utf8"));
const payload = JSON.parse(vector.body_utf8);
const eventOccurredAt = Date.parse(payload.events[0].occurred_at);
const workerClock = Number(vector.signed_at_unix) * 1_000;
const eventAge = workerClock - eventOccurredAt;

test("the frozen producer fixture is temporally coherent for one Worker clock", () => {
  expect(vector.signed_at_unix).toBe(1787661240);
  expect(payload.events[0].occurred_at).toBe("2026-08-25T12:34:00Z");
  expect(new Date(workerClock).toISOString()).toBe("2026-08-25T12:34:00.000Z");
  expect(eventAge).toBeGreaterThanOrEqual(-5 * 60 * 1_000);
  expect(eventAge).toBeLessThanOrEqual(48 * 60 * 60 * 1_000);
  expect(vector.body_utf8).toContain('"blame_pro_protocol_version":3');
  expect(vector.body_utf8).toContain('"blame_target_kind":"file"');
  expect(vector.body_utf8).not.toMatch(/"blame_protocol_version"/u);
  expect(vector.body_utf8).not.toMatch(
    /"blame_target_(?:value|path|ref|hash|selector)"/u,
  );
});

test(
  "admits exact proof-verified Rust bytes through worker.fetch with one clock",
  async () => {
    const clock = new Date(workerClock);
    const harness = workerHarness({ now: clock });
    const headers = new Headers({
      "content-type": "application/json; charset=utf-8",
      [BLAME_INSTALLATION_KEY_HEADER]: vector.installation_public_key_base64url,
      [BLAME_PROOF_EVENT_ID_HEADER]: vector.event_id,
      [BLAME_PROOF_NONCE_HEADER]: vector.nonce_base64url,
      [BLAME_PROOF_SIGNATURE_HEADER]: vector.signature_base64url,
      [BLAME_PROOF_TIME_HEADER]: String(vector.signed_at_unix),
    });
    const request = new Request(
      `https://cli.ctx.rs${vector.path}${vector.query}`,
      { body: vector.body_utf8, headers, method: vector.method },
    );

    const response = await harness.worker.fetch(request, ENV);

    expect(response.status).toBe(204);
    expect(harness.createDatabaseClient).not.toHaveBeenCalled();
    expect((await harness.queueMessages())[0]).toMatchObject({
      kind: "blame_product_receipt",
      receipt: {
      event_id: vector.event_id,
      received_at: clock.toISOString(),
      identity_key_version: 7,
      replay_fingerprint: expect.stringMatching(/^[0-9a-f]{64}$/u),
      subject_hash: expect.stringMatching(/^[0-9a-f]{64}$/u),
      properties: expect.objectContaining({ blame_surface: "cli" }),
      },
    });
  },
);
