// Conditional R2 behavior with qualification prebound; the real absent-identity gate
// is tested by frozen_bridge_publication_test.mjs, with no production override.
import assert from "node:assert/strict";
import { mock } from "node:test";
import bridge from "../frozen-cli-bridge.cjs";
let qualifications = 0;
mock.module(new URL("../frozen-cli-bridge.cjs", import.meta.url).href, {
  namedExports: { ...bridge, assertFrozenBridgePromotion: async (version) => {
    bridge.assertCurrentReleaseVersion(version); qualifications += 1;
  } },
});
const { promoteCurrentPointer } = await import("../publish-hosted-managed-pair-stable.mjs");
const OLD = "releases/stable/current.json";
const CURRENT = "releases/stable/current-v2.json";
function pointer(version = "1.3.2", extra = {}) {
  return Buffer.from(`${JSON.stringify({
    channel: "stable", contract: "ctx-cli-release-pointer", schema_version: 1, version,
    metadata_object: `releases/stable/${version}/ctx-release-metadata.env`, metadata_sha256: "a".repeat(64),
    signature_object: `releases/stable/${version}/ctx-release-metadata.env.sig`, signature_sha256: "b".repeat(64),
    ...extra,
  })}\n`);
}
function storage(entries = [], rejectKey) {
  const objects = new Map(entries);
  const puts = [];
  const request = async (method, bucket, key, body, headers) => {
    assert.equal(bucket, "ctx-releases-prod");
    if (method === "GET") return objects.has(key)
      ? new Response(objects.get(key), { headers: { etag: '"stored"' } })
      : new Response(null, { status: 404 });
    assert.equal(method, "PUT");
    puts.push(key);
    if (key === rejectKey) return new Response(null, { status: 503 });
    assert.equal(headers["if-none-match"], undefined);
    assert.equal(headers["if-match"], '"stored"');
    objects.set(key, Buffer.from(body));
    return new Response(null, { status: 200 });
  };
  return { objects, puts, request };
}


try {
  const legacy = Buffer.from('{"latest_version":"legacy"}');
  const s = storage([[OLD, pointer()], [CURRENT, pointer()], ["releases/stable/latest.json", legacy]]);
  assert.equal(await promoteCurrentPointer(s.request, { version: "1.3.3" }, pointer("1.3.3")), "promoted");
  assert.deepEqual(s.puts, [CURRENT]);
  assert.deepEqual(s.objects.get(OLD), pointer());
  assert.deepEqual(s.objects.get("releases/stable/latest.json"), legacy);
  s.puts.length = 0;
  assert.equal(await promoteCurrentPointer(s.request, { version: "1.3.3" }, pointer("1.3.3")), "existing-identical");
  assert.deepEqual(s.puts, []);
  for (const original of [undefined, Buffer.from("bad-json"), pointer("1.3.4"), pointer("1.3.3", { metadata_sha256: "c".repeat(64) })]) {
    const invalid = storage(original === undefined ? [[OLD, pointer()]] : [[OLD, pointer()], [CURRENT, original]]);
    await assert.rejects(promoteCurrentPointer(invalid.request, { version: "1.3.3" }, pointer("1.3.3")));
    assert.deepEqual(invalid.puts, []);
    assert.deepEqual(invalid.objects.get(OLD), pointer());
  }
  const failed = storage([[OLD, pointer()], [CURRENT, pointer()]], CURRENT);
  await assert.rejects(promoteCurrentPointer(failed.request, { version: "1.3.3" }, pointer("1.3.3")), /PUT failed/u);
  assert.deepEqual(failed.objects.get(CURRENT), pointer());
  assert.deepEqual(failed.objects.get(OLD), pointer());
  const resumed = storage([...failed.objects]);
  await promoteCurrentPointer(resumed.request, { version: "1.3.3" }, pointer("1.3.3"));
  assert.deepEqual(resumed.puts, [CURRENT]);
  const raced = storage([[OLD, pointer()], [CURRENT, pointer()]]);
  let conditionalFailures = 0;
  const racingRequest = (method, ...args) => {
    if (method === "PUT" && conditionalFailures++ === 0) return Promise.resolve(new Response(null, { status: 412 }));
    return raced.request(method, ...args);
  };
  assert.equal(await promoteCurrentPointer(racingRequest, { version: "1.3.3" }, pointer("1.3.3")), "promoted");
  assert.equal(conditionalFailures, 2);

  // A failed conditional write must reread the winner before retrying L.
  const advanced = storage([[OLD, pointer()], [CURRENT, pointer()]]);
  const newer = pointer("1.3.4");
  const staleRequests = [];
  const advancingRequest = async (method, bucket, key, body, headers) => {
    staleRequests.push([method, key]);
    if (method === "PUT") {
      assert.equal(key, CURRENT);
      assert.equal(headers["if-match"], '"stored"');
      assert.deepEqual(body, pointer("1.3.3"));
      assert.deepEqual(advanced.objects.get(CURRENT), pointer());
      advanced.objects.set(CURRENT, newer);
      return new Response(null, { status: 412 });
    }
    const response = await advanced.request(method, bucket, key, body, headers);
    if (key === CURRENT && advanced.objects.get(CURRENT) === newer) response.headers.set("etag", '"newer"');
    return response;
  };
  await assert.rejects(promoteCurrentPointer(advancingRequest, { version: "1.3.3" }, pointer("1.3.3")),
    /stable pointer cannot be replaced by this release/u);
  assert.deepEqual(staleRequests, [["GET", CURRENT], ["PUT", CURRENT], ["GET", CURRENT]]);
  assert.deepEqual(advanced.objects.get(CURRENT), newer);
  assert.deepEqual(advanced.objects.get(OLD), pointer());

  let alwaysRaces = 0;
  const exhausted = storage([[OLD, pointer()], [CURRENT, pointer()]]);
  const exhaustingRequest = (method, ...args) => {
    if (method === "PUT") { alwaysRaces += 1; return Promise.resolve(new Response(null, { status: 412 })); }
    return exhausted.request(method, ...args);
  };
  await assert.rejects(promoteCurrentPointer(exhaustingRequest, { version: "1.3.3" }, pointer("1.3.3")), /changed concurrently too many times/u);
  assert.equal(alwaysRaces, 4);
  assert.deepEqual(exhausted.objects.get(CURRENT), pointer());
  assert.deepEqual(exhausted.objects.get(OLD), pointer());

  // The retained v2 feed must also reject readback corruption and unbounded GETs.
  const corrupted = storage([[OLD, pointer()], [CURRENT, pointer()]]);
  let wrote = false;
  const corruptReadback = async (method, ...args) => {
    if (method === "GET" && wrote) return new Response(pointer("1.3.4"));
    const response = await corrupted.request(method, ...args);
    if (method === "PUT") wrote = true;
    return response;
  };
  await assert.rejects(promoteCurrentPointer(corruptReadback, { version: "1.3.3" }, pointer("1.3.3")), /readback failed/u);
  assert.deepEqual(corrupted.objects.get(OLD), pointer());
  let cancelled = false;
  const oversized = async (method, bucket, key) => {
    assert.equal(method, "GET"); assert.equal(key, CURRENT);
    return new Response(new ReadableStream({
      pull(controller) { controller.enqueue(new Uint8Array(16 * 1024 + 1)); },
      cancel() { cancelled = true; },
    }));
  };
  await assert.rejects(promoteCurrentPointer(oversized, { version: "1.3.3" }, pointer("1.3.3")), /streamed body bound/u);
  assert.equal(cancelled, true);
  assert.ok(qualifications >= 8);
} finally { mock.restoreAll(); }
