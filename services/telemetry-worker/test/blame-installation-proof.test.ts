import { describe, expect, test, vi } from "vitest";

import {
  BLAME_INSTALLATION_KEY_HEADER,
  BLAME_PROOF_EVENT_ID_HEADER,
  BLAME_PROOF_NONCE_HEADER,
  BLAME_PROOF_SIGNATURE_HEADER,
  BLAME_PROOF_TIME_HEADER,
  blameInstallationProofTranscript,
  verifiedBlameInstallationCoordinate,
} from "../src/blame-installation-proof";
import { NeonTelemetryDatabase } from "../src/database";
import { hmacSha256Hex } from "../src/hash";
import { buildTelemetryIngestPlan } from "../src/telemetry-ingest";
import { buildBlameProductReceipt } from "../src/blame-product-receipt";
import {
  ENV,
  EVENT_ID,
  HMAC_KEY,
  INGEST_OPTIONS,
  NOW,
  neonHarness,
  proOperationEvent,
  v1Batch,
  workerHarness,
} from "./worker-test-fixtures";

describe("verified installation proof", () => {
  test("binds exact body, sole event, method, path, empty query, time, and nonce", async () => {
    const signed = await signedRequest();
    await expect(verifiedBlameInstallationCoordinate(signed.verification)).resolves.toMatch(
      /^[A-Za-z0-9_-]{43}$/u,
    );

    for (const mutation of [
      { method: "PUT" },
      { path: "/functions/v1/telemetry" },
      { query: "?debug=1" },
      { now: new Date(NOW.getTime() + 121_000) },
      { body: new TextEncoder().encode(`${signed.body} `) },
      { payload: v1Batch([currentBlameEvent({ event_id: "44444444-4444-4444-8444-444444444444" })]) },
    ]) {
      await expect(verifiedBlameInstallationCoordinate({
        ...signed.verification,
        ...mutation,
      })).resolves.toBeUndefined();
    }
  });

  test("rejects current Blame when proof is absent, invalid, or body-mutated", async () => {
    const payload = v1Batch([currentBlameEvent()]);
    const unsignedHarness = workerHarness();
    const unsigned = await unsignedHarness.worker.fetch(request(JSON.stringify(payload)), ENV);
    expect(unsigned.status).toBe(422);
    expect(await unsigned.json()).toEqual({ error: "blame_installation_proof_required" });
    expect(unsignedHarness.queueSendBatch).not.toHaveBeenCalled();

    const signed = await signedRequest(payload);
    const mutatedPayload = structuredClone(payload);
    properties(mutatedPayload).blame_has_more = true;
    const invalidHarness = workerHarness();
    const invalid = await invalidHarness.worker.fetch(
      request(JSON.stringify(mutatedPayload), signed.headers),
      ENV,
    );
    expect(invalid.status).toBe(422);
    expect(await invalid.json()).toEqual({ error: "blame_installation_proof_required" });
    expect(invalidHarness.queueSendBatch).not.toHaveBeenCalled();
  });

  test("valid proof admits one isolated receipt and awaits durable queue send", async () => {
    let admit: (() => void) | undefined;
    const pendingAdmission = new Promise<void>((resolve) => { admit = resolve; });
    const harness = workerHarness({ queuePromise: pendingAdmission });
    const signed = await signedRequest(identitylessBatch([currentBlameEvent()]));
    const responsePromise = harness.worker.fetch(request(signed.body, signed.headers), ENV);

    await vi.waitFor(() => expect(harness.queueSendBatch).toHaveBeenCalledOnce());
    expect(harness.createDatabaseClient).not.toHaveBeenCalled();
    const message = (await harness.queueMessages())[0];
    expect(message).toMatchObject({
      kind: "blame_product_receipt",
      receipt: {
      event_id: EVENT_ID,
      identity_key_version: 7,
      replay_fingerprint: expect.stringMatching(/^[0-9a-f]{64}$/u),
      subject_hash: expect.stringMatching(/^[0-9a-f]{64}$/u),
      properties: expect.objectContaining({ blame_surface: "cli" }),
      },
    });
    let settled = false;
    void responsePromise.then(() => { settled = true; });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(settled).toBe(false);
    admit!();
    expect((await responsePromise).status).toBe(204);
  });

  test("valid proof admits one schema V2 receipt with the released subject", async () => {
    const payload = identitylessBatch([currentBlameV2Event()]);
    payload.app_version = "1.2.3";
    const signed = await signedRequest(payload);
    const coordinate = await verifiedBlameInstallationCoordinate(signed.verification);
    const harness = workerHarness();

    const response = await harness.worker.fetch(request(signed.body, signed.headers), ENV);

    expect(response.status).toBe(204);
    expect(harness.createDatabaseClient).not.toHaveBeenCalled();
    expect((await harness.queueMessages())[0]).toMatchObject({
      kind: "blame_product_receipt",
      receipt: {
      activity_class: "product_value",
      app_version: "1.2.3",
      arch: "x86_64",
      duration_bucket: "lt_1s",
      event_id: EVENT_ID,
      os: "linux",
      properties: expect.objectContaining({
        blame_result_count_bucket: "0",
        blame_result_state: "none",
        blame_schema_version: 2,
        blame_surface: "mcp",
      }),
      subject_hash: await hmacSha256Hex(
        HMAC_KEY,
        "ctx.telemetry.blame-subject.v1.key-7",
        coordinate!,
      ),
      },
    });
  });

  test("requires the released proof for an identified schema V2 envelope", async () => {
    const payload = v1Batch([currentBlameV2Event()]);
    payload.app_version = "1.2.3";
    const harness = workerHarness();

    const response = await harness.worker.fetch(request(JSON.stringify(payload)), ENV);

    expect(response.status).toBe(422);
    expect(await response.json()).toEqual({ error: "blame_installation_proof_required" });
    expect(harness.queueSendBatch).not.toHaveBeenCalled();
  });

  test("valid proof rejects an identified current envelope instead of entering generic storage", async () => {
    const payload = v1Batch([currentBlameEvent()]);
    const signed = await signedRequest(payload);
    const harness = workerHarness();

    const response = await harness.worker.fetch(request(signed.body, signed.headers), ENV);

    expect(response.status).toBe(422);
    expect(await response.json()).toEqual({ error: "blame_installation_proof_required" });
    expect(harness.queueSendBatch).not.toHaveBeenCalled();
  });

  test("admits one signed identityless materialization terminal after durable queue send", async () => {
    let admit: (() => void) | undefined;
    const pendingAdmission = new Promise<void>((resolve) => { admit = resolve; });
    const payload = producerMaterializationBatch();
    const signed = await signedRequest(payload);
    const coordinate = await verifiedBlameInstallationCoordinate(signed.verification);
    const harness = workerHarness({ queuePromise: pendingAdmission });
    const responsePromise = harness.worker.fetch(request(signed.body, signed.headers), ENV);

    await vi.waitFor(() => expect(harness.queueSendBatch).toHaveBeenCalledOnce());
    expect(harness.createDatabaseClient).not.toHaveBeenCalled();
    const message = (await harness.queueMessages())[0];
    if (message?.kind !== "telemetry_row") throw new Error("expected_row");
    const row = message.row;
    expect(row).toMatchObject({
      client_profile_id_hash: await hmacSha256Hex(
        HMAC_KEY,
        "ctx.telemetry.pro-materialization.client-profile.v1.key-7",
        coordinate!,
      ),
      data_root_id_hash: await hmacSha256Hex(
        HMAC_KEY,
        "ctx.telemetry.pro-materialization.data-root.v1.key-7",
        coordinate!,
      ),
      identity_key_version: 7,
      properties: expect.objectContaining({
        materialization_commit: "committed",
        materialization_freshness: "current",
        materialization_result: "completed",
      }),
    });
    for (const key of [
      "materialization_mode",
      "materialization_batch_count_bucket",
      "materialization_input_count_bucket",
      "materialization_output_count_bucket",
      "materialization_lag_bucket",
    ]) expect(row?.properties).not.toHaveProperty(key);
    expect(JSON.stringify(row)).not.toContain(coordinate!);

    let settled = false;
    void responsePromise.then(() => { settled = true; });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(settled).toBe(false);
    admit!();
    expect((await responsePromise).status).toBe(204);
  });

  test("retains signed identityless materialization failure handling", async () => {
    const payload = producerMaterializationBatch({
      outcome: "failure",
      properties: {
        helper_connection_outcome: "timed_out",
        materialization_commit: "not_committed",
        materialization_failure_bucket: "helper_timeout",
        materialization_freshness: "unknown",
        materialization_result: "failed",
      },
    });
    const signed = await signedRequest(payload);
    const harness = workerHarness();

    const response = await harness.worker.fetch(request(signed.body, signed.headers), ENV);

    expect(response.status).toBe(204);
    const message = (await harness.queueMessages())[0];
    if (message?.kind !== "telemetry_row") throw new Error("expected_row");
    expect(message.row).toMatchObject({
      status: "failure",
      success: false,
      client_profile_id_hash: expect.stringMatching(/^[0-9a-f]{64}$/u),
      data_root_id_hash: expect.stringMatching(/^[0-9a-f]{64}$/u),
      properties: expect.objectContaining({ materialization_failure_bucket: "helper_timeout" }),
    });
  });

  test("requires one valid proof-authorized identityless materialization event", async () => {
    const payload = producerMaterializationBatch();
    const unsignedHarness = workerHarness();
    const unsigned = await unsignedHarness.worker.fetch(request(JSON.stringify(payload)), ENV);
    expect(unsigned.status).toBe(422);
    expect(await unsigned.json()).toEqual({ error: "materialization_installation_proof_required" });

    const signed = await signedRequest(payload);
    const invalidHarness = workerHarness();
    const invalid = await invalidHarness.worker.fetch(
      request(`${signed.body} `, signed.headers),
      ENV,
    );
    expect(invalid.status).toBe(422);
    expect(await invalid.json()).toEqual({ error: "materialization_installation_proof_required" });

    const twoEvents = identitylessBatch([
      producerMaterializationEvent(),
      producerMaterializationEvent({ event_id: "44444444-4444-4444-8444-444444444444" }),
    ]);
    await expect(buildTelemetryIngestPlan(twoEvents, {
      ...INGEST_OPTIONS,
      verifiedInstallationCoordinate: "proof-coordinate",
    })).rejects.toMatchObject({ code: "invalid_materialization_proof_event_count", status: 422 });
  });

  test("rejects a signed materialization envelope that carries producer identities", async () => {
    const payload = producerMaterializationBatch();
    Object.assign(payload, {
      client_profile_id: "11111111-1111-4111-8111-111111111111",
      data_root_id: "22222222-2222-4222-8222-222222222222",
    });
    const signed = await signedRequest(payload);
    const harness = workerHarness();

    const response = await harness.worker.fetch(request(signed.body, signed.headers), ENV);

    expect(response.status).toBe(422);
    expect(await response.json()).toEqual({
      error: "materialization_installation_proof_requires_identityless",
    });
    expect(harness.queueSendBatch).not.toHaveBeenCalled();
  });

  test("rotates proof-derived materialization identities with the Worker key version", async () => {
    const signed = await signedRequest(producerMaterializationBatch());
    const currentHarness = workerHarness();
    const rotatedHarness = workerHarness();
    const rotatedEnv = { ...ENV, TELEMETRY_IDENTITY_KEY_VERSION: "8" };

    expect((await currentHarness.worker.fetch(request(signed.body, signed.headers), ENV)).status).toBe(204);
    expect((await rotatedHarness.worker.fetch(request(signed.body, signed.headers), rotatedEnv)).status).toBe(204);

    const currentMessage = (await currentHarness.queueMessages())[0];
    const rotatedMessage = (await rotatedHarness.queueMessages())[0];
    if (currentMessage?.kind !== "telemetry_row" || rotatedMessage?.kind !== "telemetry_row") {
      throw new Error("expected_rows");
    }
    const current = currentMessage.row;
    const rotated = rotatedMessage.row;
    expect(current).toMatchObject({ identity_key_version: 7 });
    expect(rotated).toMatchObject({ identity_key_version: 8 });
    expect(rotated?.client_profile_id_hash).not.toBe(current?.client_profile_id_hash);
    expect(rotated?.data_root_id_hash).not.toBe(current?.data_root_id_hash);
  });

  test("reports queue failure after a valid signed materialization plan", async () => {
    const signed = await signedRequest(producerMaterializationBatch());
    const harness = workerHarness({ queueError: new Error("queue unavailable") });

    const response = await harness.worker.fetch(request(signed.body, signed.headers), ENV);

    expect(response.status).toBe(503);
    expect(await response.json()).toEqual({ error: "queue_admission_failed" });
    expect(harness.createDatabaseClient).not.toHaveBeenCalled();
    expect(harness.observeRejection).toHaveBeenCalledWith(expect.objectContaining({
      code: "queue_admission_failed",
      rejection_class: "queue_admission",
      status: 503,
    }));
  });

  test("rejects an identityless Blame event when proof is absent or invalid", async () => {
    const payload = identitylessBatch([currentBlameEvent()]);
    const harness = workerHarness();
    const response = await harness.worker.fetch(request(JSON.stringify(payload)), ENV);
    expect(response.status).toBe(422);
    expect(await response.json()).toEqual({ error: "invalid_client_profile_id" });
    expect(harness.queueSendBatch).not.toHaveBeenCalled();
  });

  test("writes one receipt through the isolated Neon record function", async () => {
    const coordinate = "proof-coordinate";
    const plan = await buildTelemetryIngestPlan(identitylessBatch([currentBlameEvent()]), {
      ...INGEST_OPTIONS,
      verifiedInstallationCoordinate: coordinate,
    });
    const receipt = await buildBlameProductReceipt(
      plan.rows, coordinate, "worker-identity-hmac-key-for-tests", 7,
    );
    const neon = neonHarness();
    await new NeonTelemetryDatabase(neon.client).insertBlameProductReceipt(receipt!);

    expect(neon.calls).toHaveLength(1);
    expect(neon.calls[0]?.[0]).toContain("record_blame_product_receipt");
    expect(neon.calls[0]?.[1]).toEqual([JSON.stringify(receipt)]);
    expect(neon.calls[0]?.[0]).not.toContain("telemetry_event");
  });

  test("keeps identical replay stable across received weeks and changes for another proof subject", async () => {
    const replayPayload = identitylessBatch([currentBlameEvent({
      occurred_at: "2026-08-30T23:59:00Z",
    })]);
    const firstProof = await signedRequest(replayPayload);
    const otherProof = await signedRequest(replayPayload);
    const firstCoordinate = await verifiedBlameInstallationCoordinate(firstProof.verification);
    const otherCoordinate = await verifiedBlameInstallationCoordinate(otherProof.verification);
    expect(firstCoordinate).toMatch(/^[A-Za-z0-9_-]{43}$/u);
    expect(otherCoordinate).toMatch(/^[A-Za-z0-9_-]{43}$/u);
    expect(otherCoordinate).not.toBe(firstCoordinate);

    const firstPlan = await buildTelemetryIngestPlan(replayPayload, {
      ...INGEST_OPTIONS,
      verifiedInstallationCoordinate: firstCoordinate,
      now: () => new Date("2026-08-30T23:59:59Z"),
    });
    const replayPlan = await buildTelemetryIngestPlan(replayPayload, {
      ...INGEST_OPTIONS,
      verifiedInstallationCoordinate: firstCoordinate,
      now: () => new Date("2026-08-31T00:00:01Z"),
    });
    const firstReceipt = await buildBlameProductReceipt(
      firstPlan.rows, firstCoordinate, "worker-identity-hmac-key-for-tests", 7,
    );
    const replayReceipt = await buildBlameProductReceipt(
      replayPlan.rows, firstCoordinate, "worker-identity-hmac-key-for-tests", 7,
    );
    const changedProofReceipt = await buildBlameProductReceipt(
      replayPlan.rows, otherCoordinate, "worker-identity-hmac-key-for-tests", 7,
    );
    const changedVersionReceipt = await buildBlameProductReceipt(
      replayPlan.rows, firstCoordinate, "worker-identity-hmac-key-for-tests", 8,
    );

    expect(replayPlan.rows[0]?.payload_fingerprint)
      .toBe(firstPlan.rows[0]?.payload_fingerprint);
    expect(replayReceipt).toEqual({
      ...firstReceipt,
      received_at: "2026-08-31T00:00:01.000Z",
    });
    expect(changedProofReceipt?.subject_hash).not.toBe(firstReceipt?.subject_hash);
    expect(changedVersionReceipt).toMatchObject({ identity_key_version: 8 });
    expect(changedVersionReceipt?.subject_hash).not.toBe(firstReceipt?.subject_hash);
  });
});

function currentBlameEvent(overrides: Record<string, unknown> = {}) {
  return proOperationEvent("blame", {
    blame_schema_version: 1,
    blame_semantics_version: 1,
    blame_surface: "cli",
    blame_target_kind: "file",
    blame_request_kind: "first_request",
    blame_access_state: "active",
    blame_result_state: "none",
    blame_freshness: "current",
    blame_has_more: false,
    blame_output_served: true,
    blame_pro_version: "1.1.0",
    blame_pro_protocol_version: 3,
  }, overrides);
}

function currentBlameV2Event(overrides: Record<string, unknown> = {}) {
  return proOperationEvent("blame", {
    blame_schema_version: 2,
    blame_surface: "mcp",
    blame_target_kind: "pull_request",
    blame_request_kind: "continuation",
    blame_query_duration_bucket: "lt_1s",
    blame_result_state: "none",
    blame_result_count_bucket: "0",
    blame_freshness: "stale_committed",
    blame_has_more: false,
  }, overrides);
}

async function signedRequest(payload = v1Batch([currentBlameEvent()])) {
  const body = JSON.stringify(payload);
  const bodyBytes = new TextEncoder().encode(body);
  const pair = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const publicKey = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  const nonce = base64url(new Uint8Array(32).fill(7));
  const proofTime = String(Math.floor(NOW.getTime() / 1_000));
  const transcript = await blameInstallationProofTranscript({
    body: bodyBytes,
    eventId: EVENT_ID,
    method: "POST",
    nonce,
    path: "/functions/v1/analytics",
    proofTime,
  });
  const signature = new Uint8Array(await crypto.subtle.sign(
    "Ed25519",
    pair.privateKey,
    arrayBuffer(transcript),
  ));
  const headers = new Headers({
    [BLAME_INSTALLATION_KEY_HEADER]: base64url(publicKey),
    [BLAME_PROOF_EVENT_ID_HEADER]: EVENT_ID,
    [BLAME_PROOF_NONCE_HEADER]: nonce,
    [BLAME_PROOF_SIGNATURE_HEADER]: base64url(signature),
    [BLAME_PROOF_TIME_HEADER]: proofTime,
  });
  return {
    body,
    headers,
    verification: {
      body: bodyBytes,
      headers,
      method: "POST",
      now: NOW,
      path: "/functions/v1/analytics",
      payload,
      query: "",
    },
  };
}

function identitylessBatch(events: Record<string, unknown>[]): Record<string, unknown> {
  return {
    app_version: "0.26.0",
    os: "linux",
    arch: "x86_64",
    events,
  };
}

function producerMaterializationBatch(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  const payload = identitylessBatch([producerMaterializationEvent(overrides)]);
  payload.app_version = "1.1.0";
  return payload;
}

function producerMaterializationEvent(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  const event = proOperationEvent("materialize", {
    helper_connection_outcome: "connected",
    materialization_batch_count_bucket: "1",
    materialization_commit: "committed",
    materialization_freshness: "current",
    materialization_input_count_bucket: "6-20",
    materialization_lag_bucket: "0",
    materialization_mode: "incremental",
    materialization_output_count_bucket: "6-20",
    materialization_result: "completed",
  });
  Object.assign(event, overrides);
  return event;
}

function request(body: string, proofHeaders = new Headers()) {
  const headers = new Headers(proofHeaders);
  headers.set("content-type", "application/json; charset=utf-8");
  return new Request("https://cli.ctx.rs/functions/v1/analytics", {
    body,
    headers,
    method: "POST",
  });
}

function properties(payload: Record<string, unknown>) {
  const events = payload.events as Record<string, unknown>[];
  return events[0]!.properties as Record<string, unknown>;
}

function base64url(value: Uint8Array) {
  let binary = "";
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/u, "");
}

function arrayBuffer(value: Uint8Array): ArrayBuffer {
  const output = new ArrayBuffer(value.byteLength);
  new Uint8Array(output).set(value);
  return output;
}
