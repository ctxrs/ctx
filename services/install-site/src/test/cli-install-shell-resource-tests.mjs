import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createServer } from "node:https";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { gzipSync } from "node:zlib";
import { shellAcquisitionFixture } from "./cli-install-resource-fixture.mjs";
import { renderCliInstallScript } from "../cli-install-script.js";

function run(command, args, env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { env, stdio: ["ignore", "pipe", "pipe"] });
    let output = "";
    child.stdout.on("data", (data) => { output += data; });
    child.stderr.on("data", (data) => { output += data; });
    const timer = setTimeout(() => child.kill("SIGKILL"), 20_000);
    child.on("error", reject);
    child.on("close", (status, signal) => { clearTimeout(timer); resolve({ status, signal, output }); });
  });
}

test("hosted shell bounds real HTTPS, retries and gzip before publication", { timeout: 60_000 }, async (t) => {
  const root = mkdtempSync(path.join(tmpdir(), "ctx-installer-bounds-"));
  const raw = Buffer.alloc(65536, 42);
  // Independent expected ceilings for the nominal 74,784-byte gzip allowance.
  const roundedGzipCap = process.platform === "darwin" ? 75776 : 75264;
  const counts = new Map();
  const peerClosed = new Map();
  const key = path.join(root, "key.pem"), cert = path.join(root, "cert.pem");
  let server;
  try {
    // Fixture CA is supplied only to curl, never installed in host trust.
    const config = path.join(root, "openssl.cnf");
    writeFileSync(config, "[req]\ndistinguished_name=dn\n[dn]\n[v3]\nsubjectAltName=DNS:localhost\nbasicConstraints=critical,CA:TRUE\nkeyUsage=keyCertSign,digitalSignature,keyEncipherment\n");
    const generated = spawnSync("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes",
      "-keyout", key, "-out", cert, "-days", "1", "-subj", "/CN=localhost",
      "-config", config, "-extensions", "v3"], { encoding: "utf8" });
    assert.equal(generated.status, 0, generated.stderr);
    server = createServer({ key: readFileSync(key), cert: readFileSync(cert) }, (req, res) => {
      const route = req.url;
      counts.set(route, (counts.get(route) ?? 0) + 1);
      peerClosed.set(route, new Promise((resolve) => res.on("close", resolve)));
      if (route === "/retry" || (route === "/recover" && counts.get(route) === 1)) {
        res.writeHead(503); res.end(); return;
      }
      if (route === "/retry-after") { res.writeHead(503, { "retry-after": "86400" }); res.end(); return; }
      if (route === "/missing.gz") { res.writeHead(404); res.end(); return; }
      if (route === "/stall") { res.writeHead(200); res.flushHeaders(); return; }
      if (route === "/slow" || route === "/trickle") {
        res.writeHead(200); res.flushHeaders();
        let offset = 0;
        const step = route === "/slow" ? 4096 : 1;
        const timer = setInterval(() => {
          res.write(raw.subarray(offset, offset + step)); offset += step;
          if (offset >= raw.length) { clearInterval(timer); res.end(); }
        }, 20);
        res.on("close", () => clearInterval(timer)); return;
      }
      let body = raw;
      if (route === "/overflow" || route === "/unknown") body = Buffer.alloc(65537, 42);
      if (route === "/gzip-cap") body = Buffer.alloc(74784, 42);
      if (route === "/rounded-cap") body = Buffer.alloc(roundedGzipCap, 42);
      if (route === "/rounded-over") body = Buffer.alloc(roundedGzipCap + 1, 42);
      if (route === "/good.gz") body = gzipSync(raw);
      if (route === "/bomb.gz") body = gzipSync(Buffer.alloc(8 * 1024 * 1024));
      if (route === "/corrupt.gz") body = Buffer.from("not gzip");
      if (route === "/truncated.gz") body = gzipSync(raw).subarray(0, -8);
      if (route === "/transport-overflow.gz") body = Buffer.alloc(80 * 1024);
      res.writeHead(200, route === "/unknown" ? {} : { "content-length": body.length });
      res.end(body);
    });
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    const base = `https://localhost:${server.address().port}`;
    const env = { ...process.env, HOME: root, CURL_HOME: root, CURL_CA_BUNDLE: cert,
      NO_PROXY: "localhost", no_proxy: "localhost", CTX_UPGRADE_AUTO: "off" };
    const owner = shellAcquisitionFixture();
    // Scale only fixed byte allowances for cheap overflow regressions. HTTP
    // cases call the real owner's explicit deadline parameter. No fake curl.
    const scaled = owner.replaceAll("268435456", "65536").replaceAll("306184224", "74784");
    const acquire = path.join(root, "acquire.sh");
    writeFileSync(acquire, `set -eu\nfail() { echo "$*" >&2; exit 1; }\n${scaled}\n` +
      'if [ "$1" = artifact ]; then shift; download_release_artifact "$@"; else shift; download_file "$@"; fi\n');
    let index = 0;
    for (const shell of ["/bin/sh", "/bin/bash"]) {
      for (const route of ["exact", "overflow", "unknown"]) {
        const dest = path.join(root, `download-${index++}`);
        const result = await run(shell, [acquire, "file", `${base}/${route}`, dest, "65536", "2"], env);
        assert.equal(result.status === 0, route === "exact", `${shell}/${route}: ${result.output}`);
        assert.equal(statSync(dest).size, 65536, `${shell}/${route} write cap`);
      }
      for (const route of ["gzip-cap", "rounded-cap", "rounded-over"]) {
        const dest = path.join(root, `download-${index++}`);
        const result = await run(shell, [acquire, "file", `${base}/${route}`, dest, "74784", "2"], env);
        assert.equal(result.status === 0, route !== "rounded-over", `${shell}/${route}: ${result.output}`);
        assert.equal(statSync(dest).size, route === "gzip-cap" ? 74784 : roundedGzipCap, `${shell}/${route} rounded write cap`);
      }
    }
    for (const route of ["slow", "stall", "trickle", "recover", "retry", "retry-after"]) {
      const dest = path.join(root, `download-${index++}`);
      const started = performance.now();
      // curl accepts fractions for max-time, but retry-max-time needs integers.
      const timeout = ["stall", "trickle", "retry-after"].includes(route) ? "1" : "10";
      const result = await run("/bin/sh", [acquire, "file", `${base}/${route}`, dest, "65536", timeout], env);
      const elapsed = performance.now() - started;
      assert.ok(counts.get(`/${route}`) > 0, `${route}: request must reach HTTPS fixture: ${result.output}`);
      const accepted = ["slow", "recover"].includes(route);
      assert.equal(result.status === 0, accepted, `${route}: ${result.output}`);
      if (accepted) assert.deepEqual(readFileSync(dest), raw);
      if (["stall", "trickle", "retry-after"].includes(route)) assert.ok(elapsed < 2500, result.output);
      if (["stall", "trickle"].includes(route)) {
        assert.ok(elapsed >= 800, `${route}: must wait for the one-second body deadline, got ${elapsed}ms`);
        let closeTimer;
        try {
          const closed = await Promise.race([peerClosed.get(`/${route}`).then(() => true),
            new Promise((resolve) => { closeTimer = setTimeout(() => resolve(false), 1000); })]);
          assert.equal(closed, true, `${route}: timed-out transfer must close the peer response`);
        } finally { clearTimeout(closeTimer); }
      }
      if (route === "retry-after") assert.equal(counts.get("/retry-after"), 1);
      if (route === "recover") assert.equal(counts.get("/recover"), 2);
      if (route === "retry") assert.equal(counts.get("/retry"), 4);
    }
    for (const route of ["good", "missing", "transport-overflow", "bomb", "corrupt", "truncated"]) {
      const dest = path.join(root, `artifact-${index++}`);
      const result = await run("/bin/sh", [acquire, "artifact", `${base}/${route}`, dest], env);
      const accepted = ["good", "missing", "transport-overflow"].includes(route);
      assert.equal(result.status === 0, accepted, `${route}: ${result.output}`);
      assert.ok(statSync(dest).size <= 65536, route);
      if (accepted) assert.deepEqual(readFileSync(dest), raw);
      else assert.equal(counts.get(`/${route}`) ?? 0, 0, "invalid expansion must not fall back");
    }
    // Existing cleanup, with presentation callbacks stubbed, remains the owner.
    const body = renderCliInstallScript();
    const cleanup = body.slice(body.indexOf("cleanup() {"), body.indexOf("if [ \"${CTX_INSTALL_NO_MAN", body.indexOf("cleanup() {")));
    assert.match(cleanup, /rm -rf "\$tmp_dir"/);
    const work = path.join(root, "loader"); mkdirSync(work);
    const sentinel = path.join(root, "installed"); writeFileSync(sentinel, "existing install");
    const cleanupScript = path.join(root, "cleanup.sh");
    writeFileSync(cleanupScript, `set -eu\n${scaled}\n${cleanup}\n` +
      'stop_install_animation() { :; }\nreport_install_stage() { :; }\nfail() { exit 1; }\n' +
      'tmp_dir="$1"; marker_tmp_path=""; binary_tmp_path=""; integration_sidecar_tmp_path=""\n' +
      'download_release_artifact "$2/bomb" "$tmp_dir/raw"\nprintf published >"$3"\n');
    const rejected = await run("/bin/sh", [cleanupScript, work, base, sentinel], env);
    assert.equal(rejected.status, 1, rejected.output);
    assert.throws(() => statSync(work), { code: "ENOENT" });
    assert.equal(readFileSync(sentinel, "utf8"), "existing install");
    t.diagnostic("25 real HTTPS cases: sh/Bash exact+overflow and gzip allowance rounding, slow/stall/trickle, retry3+recovery/Retry-After, gzip/raw/fallback/bomb/corruption, cleanup before publication");
  } finally {
    if (server) { server.closeAllConnections(); await new Promise((resolve) => server.close(resolve)); }
    rmSync(root, { recursive: true, force: true });
  }
});
