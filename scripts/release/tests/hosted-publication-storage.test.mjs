import assert from "node:assert/strict";
import test from "node:test";
import { createR2Request, getR2Object, putImmutableR2Object } from "../core-r2.mjs";

const bucket = "ctx-releases-prod";
const object = { key: "artifacts/stable/1.5.0/ctx", body: Buffer.from("authored fixture"),
  contentType: "application/octet-stream" };
function storage({ initial, putStatus = 200, readback = object.body } = {}) {
  let stored = initial;
  const writes = [];
  return { writes, request: async (method, selectedBucket, key, body, headers) => {
    assert.equal(selectedBucket, bucket); assert.equal(key, object.key);
    if (method === "GET") return stored === undefined ? new Response(null, { status: 404 }) : new Response(stored);
    assert.equal(method, "PUT");
    assert.equal(headers["if-none-match"], "*");
    assert.equal(headers["if-match"], undefined);
    assert.deepEqual(body, object.body);
    writes.push(key);
    if ([200, 412].includes(putStatus)) stored = readback;
    return new Response(null, { status: putStatus });
  } };
}

test("immutable publication retries identical objects and verifies conditional winners", async () => {
  const created = storage();
  assert.equal(await putImmutableR2Object(created.request, bucket, object), "created");
  assert.equal(await putImmutableR2Object(created.request, bucket, object), "existing-identical");
  assert.deepEqual(created.writes, [object.key]);
  const raced = storage({ putStatus: 412 });
  assert.equal(await putImmutableR2Object(raced.request, bucket, object), "raced-identical");
  const conflict = storage({ initial: Buffer.from("changed") });
  await assert.rejects(putImmutableR2Object(conflict.request, bucket, object), /already differs/);
  assert.deepEqual(conflict.writes, []);
  for (const putStatus of [200, 412]) {
    const changed = storage({ putStatus, readback: Buffer.from("changed") });
    await assert.rejects(putImmutableR2Object(changed.request, bucket, object), /verification failed/);
  }
  const failed = storage({ putStatus: 503 });
  await assert.rejects(putImmutableR2Object(failed.request, bucket, object), /PUT failed/);
});

test("R2 publication reads reject oversized declared or streamed bytes", async () => {
  const declared = async () => new Response("small", { headers: { "content-length": "9999" } });
  await assert.rejects(getR2Object(declared, bucket, object.key, 16), /declared body bound/);
  let cancelled = false;
  const streamed = async () => new Response(new ReadableStream({
    pull(controller) { controller.enqueue(new Uint8Array(17)); },
    cancel() { cancelled = true; },
  }));
  await assert.rejects(getR2Object(streamed, bucket, object.key, 16), /streamed body bound/);
  assert.equal(cancelled, true);
  await assert.rejects(getR2Object(async () => new Response("long", {
    headers: { "content-length": "1" },
  }), bucket, object.key, 16), /differs from content-length/);
});

test("public storage authority rejects foreign buckets and unsafe object paths before fetch", async () => {
  const authority = { accessKeyEnv: "TEST_ACCESS", secretKeyEnv: "TEST_SECRET",
    endpointEnv: "TEST_ENDPOINT", bucket, label: "fixture" };
  const environment = { TEST_ACCESS: "fixture-access", TEST_SECRET: "fixture-secret",
    TEST_ENDPOINT: "https://publication.invalid" };
  let requests = 0;
  const request = createR2Request(authority, environment, async (url, options) => {
    requests += 1;
    assert.equal(url.origin, environment.TEST_ENDPOINT);
    assert.equal(options.redirect, "error");
    assert.match(options.headers.authorization, /Credential=fixture-access\//);
    return new Response("fixture");
  });
  await assert.rejects(request("GET", "commercial-bucket", object.key), /unexpected R2 bucket/);
  for (const key of ["../escape", "nested/../escape", "escape%2fpart", "name\\escape", "/absolute", "double//slash"]) {
    await assert.rejects(request("GET", bucket, key), /object key is invalid/);
  }
  assert.equal(requests, 0);
  await request("GET", bucket, object.key);
  assert.equal(requests, 1);
  for (const endpoint of ["http://publication.invalid", "https://publication.invalid/path", "https://user@publication.invalid"]) {
    assert.throws(() => createR2Request(authority, { ...environment, TEST_ENDPOINT: endpoint }), /pathless HTTPS/);
  }
});
