// Conditional R2 behavior with qualification prebound; the real absent-identity gate
// is tested by frozen_bridge_publication_test.mjs, with no production override.
import assert from "node:assert/strict";
import crypto from "node:crypto";
import { mock } from "node:test";
import bridge from "../frozen-cli-bridge.cjs";
let qualifications = 0;
mock.module(new URL("../frozen-cli-bridge.cjs", import.meta.url).href, {
  namedExports: { ...bridge, assertFrozenBridgePromotion: async (version) => {
    bridge.assertCurrentReleaseVersion(version); qualifications += 1;
  } },
});
const { privateKey, publicKey } = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
mock.module(new URL("../../../services/install-site/src/cli-install-script.js", import.meta.url).href, {
  namedExports: { CLI_METADATA_PUBLIC_KEY_PEM: publicKey.export({ format: "pem", type: "spki" }) },
});
const { promoteCurrentPointer, promoteTransitionPointer, promoteCompatiblePointer, cutOverLegacyPointer, validatePromotion, pointerBytes } = await import("../publish-hosted-managed-pair-stable.mjs");
const OLD = "releases/stable/current.json";
const CURRENT = "releases/stable/current-v3.json";
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
    if (objects.has(key)) {
      assert.equal(headers["if-none-match"], undefined);
      assert.equal(headers["if-match"], '"stored"');
    } else {
      assert.equal(headers["if-none-match"], "*");
      assert.equal(headers["if-match"], undefined);
    }
    objects.set(key, Buffer.from(body));
    return new Response(null, { status: 200 });
  };
  return { objects, puts, request };
}


try {
  const legacy = Buffer.from('{"latest_version":"legacy"}');
  const s = storage([[OLD, pointer()], [CURRENT, pointer()], ["releases/stable/latest.json", legacy]]);
  assert.equal(await promoteCurrentPointer(s.request, { version: "2.0.6" }, pointer("2.0.6")), "promoted");
  assert.deepEqual(s.puts, [CURRENT]);
  assert.deepEqual(s.objects.get(OLD), pointer());
  assert.deepEqual(s.objects.get("releases/stable/latest.json"), legacy);
  s.puts.length = 0;
  assert.equal(await promoteCurrentPointer(s.request, { version: "2.0.6" }, pointer("2.0.6")), "existing-identical");
  assert.deepEqual(s.puts, []);
  for (const original of [Buffer.from("bad-json"), pointer("2.0.7"), pointer("2.0.6", { metadata_sha256: "c".repeat(64) })]) {
    const invalid = storage(original === undefined ? [[OLD, pointer()]] : [[OLD, pointer()], [CURRENT, original]]);
    await assert.rejects(promoteCurrentPointer(invalid.request, { version: "2.0.6" }, pointer("2.0.6")));
    assert.deepEqual(invalid.puts, []);
    assert.deepEqual(invalid.objects.get(OLD), pointer());
  }
  const failed = storage([[OLD, pointer()], [CURRENT, pointer()]], CURRENT);
  await assert.rejects(promoteCurrentPointer(failed.request, { version: "2.0.6" }, pointer("2.0.6")), /PUT failed/u);
  assert.deepEqual(failed.objects.get(CURRENT), pointer());
  assert.deepEqual(failed.objects.get(OLD), pointer());
  const resumed = storage([...failed.objects]);
  await promoteCurrentPointer(resumed.request, { version: "2.0.6" }, pointer("2.0.6"));
  assert.deepEqual(resumed.puts, [CURRENT]);
  const raced = storage([[OLD, pointer()], [CURRENT, pointer()]]);
  let conditionalFailures = 0;
  const racingRequest = (method, ...args) => {
    if (method === "PUT" && conditionalFailures++ === 0) return Promise.resolve(new Response(null, { status: 412 }));
    return raced.request(method, ...args);
  };
  assert.equal(await promoteCurrentPointer(racingRequest, { version: "2.0.6" }, pointer("2.0.6")), "promoted");
  assert.equal(conditionalFailures, 2);

  // A failed conditional write must reread the winner before retrying L.
  const advanced = storage([[OLD, pointer()], [CURRENT, pointer()]]);
  const newer = pointer("2.0.7");
  const staleRequests = [];
  const advancingRequest = async (method, bucket, key, body, headers) => {
    staleRequests.push([method, key]);
    if (method === "PUT") {
      assert.equal(key, CURRENT);
      assert.equal(headers["if-match"], '"stored"');
      assert.deepEqual(body, pointer("2.0.6"));
      assert.deepEqual(advanced.objects.get(CURRENT), pointer());
      advanced.objects.set(CURRENT, newer);
      return new Response(null, { status: 412 });
    }
    const response = await advanced.request(method, bucket, key, body, headers);
    if (key === CURRENT && advanced.objects.get(CURRENT) === newer) response.headers.set("etag", '"newer"');
    return response;
  };
  await assert.rejects(promoteCurrentPointer(advancingRequest, { version: "2.0.6" }, pointer("2.0.6")),
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
  await assert.rejects(promoteCurrentPointer(exhaustingRequest, { version: "2.0.6" }, pointer("2.0.6")), /changed concurrently too many times/u);
  assert.equal(alwaysRaces, 4);
  assert.deepEqual(exhausted.objects.get(CURRENT), pointer());
  assert.deepEqual(exhausted.objects.get(OLD), pointer());

  // The v3 feed must also reject readback corruption and unbounded GETs.
  const corrupted = storage([[OLD, pointer()], [CURRENT, pointer()]]);
  let wrote = false;
  const corruptReadback = async (method, ...args) => {
    if (method === "GET" && wrote) return new Response(pointer("2.0.7"));
    const response = await corrupted.request(method, ...args);
    if (method === "PUT") wrote = true;
    return response;
  };
  await assert.rejects(promoteCurrentPointer(corruptReadback, { version: "2.0.6" }, pointer("2.0.6")), /readback failed/u);
  assert.deepEqual(corrupted.objects.get(OLD), pointer());
  let cancelled = false;
  const oversized = async (method, bucket, key) => {
    assert.equal(method, "GET"); assert.equal(key, CURRENT);
    return new Response(new ReadableStream({
      pull(controller) { controller.enqueue(new Uint8Array(16 * 1024 + 1)); },
      cancel() { cancelled = true; },
    }));
  };
  await assert.rejects(promoteCurrentPointer(oversized, { version: "2.0.6" }, pointer("2.0.6")), /streamed body bound/u);
  assert.equal(cancelled, true);
  const first = storage([[OLD, pointer()]]);
  assert.equal(await promoteCurrentPointer(first.request, { version: "2.0.5" }, pointer("2.0.5")), "created");
  assert.deepEqual(first.puts, [CURRENT]);
  assert.deepEqual(first.objects.get(OLD), pointer());
  assert.ok(qualifications >= 8);
  const LEGACY = "releases/stable/current-v2.json";
  const digest = (body) => crypto.createHash("sha256").update(body).digest("hex");
  // Target names come from the public platform contract; tiny synthetic bytes
  // stand in for the already validated signed publication artifacts.
  const { HOSTED_MANAGED_PAIR_TARGETS } = await import("../hosted-managed-pair-release.mjs");
  const bridgeLoaded = { version: "1.6.5", targets: new Map(HOSTED_MANAGED_PAIR_TARGETS.map(
    ({ id }) => [id, { core: { artifact: { body: Buffer.from("bridge") } } }],
  )) };
  const meta = Buffer.from("CTX_RELEASE_VERSION=2.0.5\nCTX_RELEASE_CHANNEL=stable\n");
  const sig = Buffer.from(`${crypto.sign("RSA-SHA256", meta, privateKey).toString("base64")}\n`);
  const destination = pointerBytes({ version: "2.0.5" }, meta, sig);
  const bridgePointer = pointer("1.6.5");
  const entries = [
    [OLD, pointer()], [LEGACY, pointer("2.0.4")],
    ["releases/stable/2.0.5/ctx-release-metadata.env", meta],
    ["releases/stable/2.0.5/ctx-release-metadata.env.sig", sig],
  ];
  const migration = storage(entries);
  assert.deepEqual(await promoteTransitionPointer(migration.request, { version: "2.0.5" }, destination),
    { current: "created", legacy: "promoted" });
  assert.deepEqual(migration.puts, [CURRENT, LEGACY]);
  assert.equal(await cutOverLegacyPointer(migration.request, bridgeLoaded, bridgePointer, digest(destination)), "promoted");
  assert.deepEqual(migration.objects.get(CURRENT), destination);
  assert.deepEqual(migration.objects.get(OLD), pointer());
  migration.puts.length = 0;
  assert.equal(await cutOverLegacyPointer(migration.request, bridgeLoaded, bridgePointer, digest(destination)), "existing-identical");
  await assert.rejects(promoteTransitionPointer(migration.request, { version: "2.0.5" }, destination), /not in.*transition/u);
  await assert.rejects(promoteCurrentPointer(migration.request, bridgeLoaded, bridgePointer), /v3 feed/u);
  assert.deepEqual(migration.puts, []);

  // A concurrent cutover after the initial transition check must not be undone.
  const racingCutover = storage(entries);
  let legacyReads = 0;
  const afterCutover = async (method, bucket, key, ...args) => {
    if (method === "GET" && key === LEGACY && ++legacyReads === 2) {
      racingCutover.objects.set(LEGACY, bridgePointer);
    }
    return racingCutover.request(method, bucket, key, ...args);
  };
  await assert.rejects(promoteTransitionPointer(afterCutover, { version: "2.0.5" }, destination), /not in.*transition/u);
  assert.deepEqual(racingCutover.objects.get(LEGACY), bridgePointer);
  assert.deepEqual(racingCutover.puts, [CURRENT]);

  const retryCutover = storage(entries);
  let legacyPuts = 0;
  const onConditionalRetry = async (method, bucket, key, ...args) => {
    if (method === "PUT" && key === LEGACY) {
      legacyPuts += 1;
      retryCutover.objects.set(LEGACY, bridgePointer);
      return new Response(null, { status: 412 });
    }
    return retryCutover.request(method, bucket, key, ...args);
  };
  await assert.rejects(promoteTransitionPointer(onConditionalRetry, { version: "2.0.5" }, destination), /not in.*transition/u);
  assert.equal(legacyPuts, 1);
  assert.deepEqual(retryCutover.objects.get(LEGACY), bridgePointer);

  const ready = () => storage([...entries.filter(([key]) => key !== LEGACY), [CURRENT, destination], [LEGACY, destination]]);
  for (const alter of [
    (s) => s.objects.delete(CURRENT),
    (s) => s.objects.set(CURRENT, pointer("2.0.4")),
    (s) => s.objects.set(LEGACY, pointer("2.0.6")),
    (s) => s.objects.set("releases/stable/2.0.5/ctx-release-metadata.env", Buffer.from("changed")),
    (s) => s.objects.set("releases/stable/2.0.5/ctx-release-metadata.env.sig", Buffer.from("AAAA\n")),
  ]) {
    const invalid = ready(); alter(invalid);
    await assert.rejects(cutOverLegacyPointer(invalid.request, bridgeLoaded, bridgePointer, digest(destination)));
    assert.deepEqual(invalid.puts, []);
  }
  const wrongHash = ready();
  await assert.rejects(cutOverLegacyPointer(wrongHash.request, bridgeLoaded, bridgePointer, "a".repeat(64)), /changed/u);
  assert.deepEqual(wrongHash.puts, []);
  const wrongSigned = ready();
  const alteredMeta = Buffer.from("CTX_RELEASE_VERSION=2.0.4\nCTX_RELEASE_CHANNEL=stable\n");
  const alteredSig = Buffer.from(`${crypto.sign("RSA-SHA256", alteredMeta, privateKey).toString("base64")}\n`);
  wrongSigned.objects.set(CURRENT, pointerBytes({ version: "2.0.5" }, alteredMeta, alteredSig));
  wrongSigned.objects.set("releases/stable/2.0.5/ctx-release-metadata.env", alteredMeta);
  wrongSigned.objects.set("releases/stable/2.0.5/ctx-release-metadata.env.sig", alteredSig);
  await assert.rejects(cutOverLegacyPointer(wrongSigned.request, bridgeLoaded, bridgePointer, digest(destination)), /version\/channel/u);
  assert.deepEqual(wrongSigned.puts, []);
  const invalidSignature = ready();
  const badSig = Buffer.from(`${crypto.sign("RSA-SHA256", Buffer.from("other"), privateKey).toString("base64")}\n`);
  invalidSignature.objects.set(CURRENT, pointerBytes({ version: "2.0.5" }, meta, badSig));
  invalidSignature.objects.set("releases/stable/2.0.5/ctx-release-metadata.env.sig", badSig);
  await assert.rejects(cutOverLegacyPointer(invalidSignature.request, bridgeLoaded, bridgePointer, digest(destination)), /does not verify/u);
  assert.deepEqual(invalidSignature.puts, []);
  for (const status of [412, 503]) {
    const conflict = ready();
    const request = (method, ...args) => method === "PUT"
      ? Promise.resolve(new Response(null, { status })) : conflict.request(method, ...args);
    await assert.rejects(cutOverLegacyPointer(request, bridgeLoaded, bridgePointer, digest(destination)), /PUT failed/u);
    assert.deepEqual(conflict.objects.get(LEGACY), destination);
  }
  const corrupt = ready();
  let replaced = false;
  const badReadback = async (method, bucket, key, ...args) => {
    if (method === "GET" && key === LEGACY && replaced) return new Response(pointer("1.6.4"));
    const response = await corrupt.request(method, bucket, key, ...args);
    if (method === "PUT") replaced = true;
    return response;
  };
  await assert.rejects(cutOverLegacyPointer(badReadback, bridgeLoaded, bridgePointer, digest(destination)), /readback failed/u);
  for (const target of HOSTED_MANAGED_PAIR_TARGETS) {
    const invalid = { ...bridgeLoaded, targets: new Map(bridgeLoaded.targets) };
    invalid.targets.set(target.id, { core: { artifact: { body: { length: 128 * 1024 * 1024 + 1 } } } });
    assert.throws(() => validatePromotion(invalid, "stage"), /download limit/u);
    assert.throws(() => validatePromotion(invalid, "bridge"), /download limit/u);
  }
  validatePromotion(bridgeLoaded, "stage");
  assert.throws(() => validatePromotion({ version: "1.6.4" }, "bridge"), /only for/u);
  assert.throws(() => validatePromotion({ version: "2.0.6" }, "transition"), /only for/u);
  assert.throws(() => validatePromotion({ version: "1.6.5" }, "current"), /v3 feed/u);

  const compatible = { ...bridgeLoaded, version: "2.2.1" };
  const signedRelease = (version, metadata = Buffer.from(`CTX_RELEASE_VERSION=${version}\nCTX_RELEASE_CHANNEL=stable\n`)) => {
    const signature = Buffer.from(`${crypto.sign("RSA-SHA256", metadata, privateKey).toString("base64")}\n`);
    return { body: pointerBytes({ version }, metadata, signature), objects: [
      [`releases/stable/${version}/ctx-release-metadata.env`, metadata],
      [`releases/stable/${version}/ctx-release-metadata.env.sig`, signature],
    ] };
  };
  const recovery = signedRelease("2.2.1");
  const latest = signedRelease("2.2.2");
  const expectedLegacy = digest(bridgePointer);
  const compatibleEntries = [[OLD, pointer()], ["releases/stable/latest.json", legacy],
    [LEGACY, bridgePointer], [CURRENT, pointer("2.2.0")], ...recovery.objects, ...latest.objects];
  const promote = (s, expected = expectedLegacy) => promoteCompatiblePointer(s.request, compatible, recovery.body, expected);
  const completed = { current: "promoted", legacy: "promoted", current_version: "2.2.1",
    current_sha256: digest(recovery.body) };

  const recovering = storage(compatibleEntries);
  assert.deepEqual(await promote(recovering), completed);
  assert.deepEqual(recovering.puts, [CURRENT, LEGACY]);
  assert.deepEqual(recovering.objects.get(CURRENT), recovery.body);
  assert.deepEqual(recovering.objects.get(LEGACY), recovery.body);
  assert.deepEqual(recovering.objects.get(OLD), pointer());
  assert.deepEqual(recovering.objects.get("releases/stable/latest.json"), legacy);
  recovering.puts.length = 0;
  assert.deepEqual(await promote(recovering), { ...completed, current: "existing-identical", legacy: "existing-identical" });
  assert.deepEqual(recovering.puts, []);

  // Normal full-size releases advance v3 alone. Repeating the compatible cutover
  // authenticates that newer destination and never rewinds it to the v2 target.
  const fullSize = { ...compatible, version: "2.2.2", targets: new Map(compatible.targets) };
  fullSize.targets.set(HOSTED_MANAGED_PAIR_TARGETS[0].id, { core: { artifact: { body: { length: 200 * 1024 * 1024 } } } });
  validatePromotion(fullSize, "current");
  await promoteCurrentPointer(recovering.request, fullSize, latest.body);
  assert.deepEqual(recovering.puts, [CURRENT]);
  assert.deepEqual(recovering.objects.get(LEGACY), recovery.body);
  recovering.puts.length = 0;
  assert.deepEqual(await promote(recovering), { current: "existing-newer", legacy: "existing-identical",
    current_version: "2.2.2", current_sha256: digest(latest.body) });
  assert.deepEqual(recovering.puts, []);
  await assert.rejects(promoteTransitionPointer(recovering.request, { version: "2.0.5" }, destination), /not in.*transition/u);
  await assert.rejects(cutOverLegacyPointer(recovering.request, bridgeLoaded, bridgePointer, digest(recovery.body)), /changed/u);
  assert.deepEqual(recovering.puts, []);

  const initializeCurrent = storage(compatibleEntries.filter(([key]) => key !== CURRENT));
  assert.equal((await promote(initializeCurrent)).current, "created");
  assert.deepEqual(initializeCurrent.puts, [CURRENT, LEGACY]);

  // Every accepted partial write is resumable without changing immutable inputs
  // or the caller's expected-old-v2 digest, even after a lost PUT response.
  for (const failedAt of [CURRENT, LEGACY, "after-legacy"]) {
    const interrupted = storage(compatibleEntries, failedAt);
    const request = async (method, bucket, key, ...args) => {
      const response = await interrupted.request(method, bucket, key, ...args);
      return failedAt === "after-legacy" && method === "PUT" && key === LEGACY
        ? new Response(null, { status: 503 }) : response;
    };
    await assert.rejects(promote({ request }), /PUT failed/u);
    assert.deepEqual(interrupted.objects.get(LEGACY), failedAt === "after-legacy" ? recovery.body : bridgePointer);
    assert.deepEqual(interrupted.objects.get(CURRENT), failedAt === CURRENT ? pointer("2.2.0") : recovery.body);
    const retry = storage([...interrupted.objects]);
    await promote(retry);
    assert.deepEqual(retry.puts, failedAt === CURRENT ? [CURRENT, LEGACY] : failedAt === LEGACY ? [LEGACY] : []);
  }
  const newerResume = storage([...compatibleEntries, [CURRENT, latest.body]]);
  assert.deepEqual(await promote(newerResume), { current: "existing-newer", legacy: "promoted",
    current_version: "2.2.2", current_sha256: digest(latest.body) });
  assert.deepEqual(newerResume.puts, [LEGACY]);

  for (const legacyBody of [null, Buffer.from("bad-json"), pointer("2.2.2"),
    pointer("2.2.1", { metadata_sha256: "c".repeat(64) })]) {
    const invalid = storage(compatibleEntries);
    if (legacyBody === null) invalid.objects.delete(LEGACY);
    else invalid.objects.set(LEGACY, legacyBody);
    await assert.rejects(promote(invalid, legacyBody === null ? expectedLegacy : digest(legacyBody)));
    assert.deepEqual(invalid.puts, []);
  }
  for (const expected of ["a".repeat(64), "invalid"]) {
    const invalid = storage(compatibleEntries);
    await assert.rejects(promote(invalid, expected), /expected/u);
    assert.deepEqual(invalid.puts, []);
  }
  const wrongCandidate = storage(compatibleEntries);
  await assert.rejects(promoteCompatiblePointer(wrongCandidate.request, compatible, latest.body, expectedLegacy), /exact pointer/u);
  assert.deepEqual(wrongCandidate.puts, []);

  const legacyChanged = storage(compatibleEntries);
  let compatibleLegacyReads = 0;
  const changeBeforeCas = async (method, bucket, key, ...args) => {
    if (method === "GET" && key === LEGACY && ++compatibleLegacyReads === 2) legacyChanged.objects.set(LEGACY, pointer("1.6.6"));
    return legacyChanged.request(method, bucket, key, ...args);
  };
  await assert.rejects(promote({ request: changeBeforeCas }), /expected older legacy pointer/u);
  assert.deepEqual(legacyChanged.puts, [CURRENT]);
  assert.deepEqual(legacyChanged.objects.get(LEGACY), pointer("1.6.6"));

  for (const winner of [pointer("1.6.6"), recovery.body]) {
    const racedLegacy = storage(compatibleEntries);
    let attempts = 0;
    const request = async (method, bucket, key, body, headers) => {
      if (method === "PUT" && key === LEGACY) {
        attempts += 1;
        assert.equal(headers["if-match"], '"stored"');
        racedLegacy.objects.set(LEGACY, winner);
        return new Response(null, { status: 412 });
      }
      return racedLegacy.request(method, bucket, key, body, headers);
    };
    if (winner === recovery.body) assert.equal((await promote({ request })).legacy, "existing-identical");
    else await assert.rejects(promote({ request }), /expected older legacy pointer/u);
    assert.equal(attempts, 1);
    assert.deepEqual(racedLegacy.objects.get(LEGACY), winner);
  }

  const advancedCurrent = storage(compatibleEntries);
  const advanceDuringCas = async (method, bucket, key, body, headers) => {
    if (method === "PUT" && key === CURRENT) {
      assert.equal(headers["if-match"], '"stored"');
      advancedCurrent.objects.set(CURRENT, latest.body);
      return new Response(null, { status: 412 });
    }
    return advancedCurrent.request(method, bucket, key, body, headers);
  };
  await assert.rejects(promote({ request: advanceDuringCas }), /cannot be replaced/u);
  assert.deepEqual(advancedCurrent.objects.get(LEGACY), bridgePointer);
  assert.equal((await promote(advancedCurrent)).current, "existing-newer");
  assert.deepEqual(advancedCurrent.puts, [LEGACY]);

  for (const alter of [
    (s) => s.objects.delete(latest.objects[0][0]),
    (s) => s.objects.set(latest.objects[0][0], Buffer.from("changed")),
    (s) => {
      const wrong = signedRelease("2.2.2", Buffer.from("CTX_RELEASE_VERSION=2.2.1\nCTX_RELEASE_CHANNEL=stable\n"));
      s.objects.set(CURRENT, wrong.body);
      for (const [key, bytes] of wrong.objects) s.objects.set(key, bytes);
    },
    (s) => {
      const signature = Buffer.from(`${crypto.sign("RSA-SHA256", Buffer.from("other"), privateKey).toString("base64")}\n`);
      s.objects.set(CURRENT, pointerBytes({ version: "2.2.2" }, latest.objects[0][1], signature));
      s.objects.set(latest.objects[1][0], signature);
    },
  ]) {
    const invalid = storage([...compatibleEntries, [CURRENT, latest.body]]);
    alter(invalid);
    await assert.rejects(promote(invalid), /metadata|verify/u);
    assert.deepEqual(invalid.puts, []);
    assert.deepEqual(invalid.objects.get(LEGACY), bridgePointer);
  }

  for (const changedDestination of [signedRelease("2.0.5"),
    signedRelease("2.2.1", Buffer.from("CTX_RELEASE_VERSION=2.2.1\nCTX_RELEASE_CHANNEL=stable\nCHANGED=1\n"))]) {
    const changed = storage([...compatibleEntries, [CURRENT, recovery.body], ...changedDestination.objects]);
    let currentReads = 0;
    const request = async (method, bucket, key, ...args) => {
      if (method === "GET" && key === CURRENT && ++currentReads === 3) changed.objects.set(CURRENT, changedDestination.body);
      return changed.request(method, bucket, key, ...args);
    };
    await assert.rejects(promote({ request }), /at or above|differs from the selected compatible/u);
    assert.deepEqual(changed.puts, []);
  }
  const badLegacyReadback = storage(compatibleEntries);
  let legacyWritten = false;
  const corruptLegacyReadback = async (method, bucket, key, ...args) => {
    if (method === "GET" && key === LEGACY && legacyWritten) return new Response(bridgePointer);
    const response = await badLegacyReadback.request(method, bucket, key, ...args);
    if (method === "PUT" && key === LEGACY) legacyWritten = true;
    return response;
  };
  await assert.rejects(promote({ request: corruptLegacyReadback }), /readback failed/u);
  assert.deepEqual(badLegacyReadback.objects.get(CURRENT), recovery.body);
  assert.deepEqual(badLegacyReadback.objects.get(LEGACY), recovery.body);
  assert.equal((await promote(badLegacyReadback)).legacy, "existing-identical");

  const boundary = { ...compatible, targets: new Map(HOSTED_MANAGED_PAIR_TARGETS.map(({ id }) =>
    [id, { core: { artifact: { body: { length: 128 * 1024 * 1024 } } } }])) };
  validatePromotion(boundary, "compatible");
  validatePromotion({ ...boundary, version: "2.0.5" }, "compatible");
  for (const version of ["1.6.5", "2.0.4", "2.2.1-rc.1"]) {
    assert.throws(() => validatePromotion({ ...boundary, version }, "compatible"), /v3 feed|canonical SemVer/u);
  }
  for (const { id } of HOSTED_MANAGED_PAIR_TARGETS) {
    for (const length of [undefined, 0, 128 * 1024 * 1024 + 1]) {
      const invalid = { ...boundary, targets: new Map(boundary.targets) };
      if (length === undefined) invalid.targets.delete(id);
      else invalid.targets.set(id, { core: { artifact: { body: { length } } } });
      assert.throws(() => validatePromotion(invalid, "compatible"), /download limit/u);
    }
  }
} finally { mock.restoreAll(); }
