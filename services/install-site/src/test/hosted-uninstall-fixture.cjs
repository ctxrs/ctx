const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { createHash } = require("node:crypto");

function main() {
  const env = process.env;
  const root = path.resolve(env.CTX_UNINSTALL_FIXTURE_ROOT);
  const install = path.join(root, "ctx-test.exe");
  const nativePath = (file) => env.CTX_UNINSTALL_FIXTURE_VERBATIM_PATHS === "1" ? path.toNamespacedPath(file) : file;
  const marker = `${install}.install.json`;
  const helper = path.join(root, ".ctx-test.exe.hosted-uninstall-helper.exe");
  const journal = path.join(root, ".ctx-test.exe.hosted-install-transaction.json");
  const actor = path.resolve(process.argv[2]);
  const args = process.argv.slice(3);
  assert.ok(actor === install || actor === helper);
  const owned = (file) => {
    const relative = path.relative(root, file);
    assert.ok(relative && !relative.startsWith("..") && !path.isAbsolute(relative));
    try { assert.ok(fs.lstatSync(file).isFile()); }
    catch (error) { if (error.code !== "ENOENT") throw error; }
    return file;
  };
  const hash = (file) => createHash("sha256").update(fs.readFileSync(owned(file))).digest("hex");
  const output = (value) => fs.writeFileSync(1, `${JSON.stringify(value)}\n`);
  const daemonFiles = ["COORDINATION", "ENDPOINT", "OWNER_LOCK", "SUPERVISOR"]
    .map((name) => owned(env[`CTX_UNINSTALL_FAKE_DAEMON_${name}`]));
  fs.appendFileSync(owned(env.CTX_UNINSTALL_FAKE_LOG), `${JSON.stringify({ actor, args })}\n`);

  if (args[0] === "upgrade") {
    const action = args[2];
    const prepare = action === "uninstall-prepare";
    assert.ok(["uninstall-prepare", "uninstall-arm", "uninstall-commit"].includes(action));
    assert.equal(actor, prepare ? install : helper);
    assert.deepEqual(args, ["upgrade", "--hosted-transaction", action, "--install-path", install,
      ...(prepare ? ["--attempt-id", args[6]] : [])]);
    if (prepare) assert.ok(typeof args[6] === "string" && args[6].length > 0);
    let state = fs.existsSync(journal) ? JSON.parse(fs.readFileSync(owned(journal), "utf8")) : null;
    if (state) {
      assert.equal(state.kind, "uninstall");
      assert.equal(state.install_path, nativePath(install));
      assert.equal(state.helper_path, nativePath(helper));
      assert.ok(["prepared", "armed", "committed"].includes(state.phase));
    }
    const save = () => fs.writeFileSync(owned(journal), JSON.stringify(state), { mode: 0o600 });
    const verify = (file, digest) => assert.equal(hash(file), digest);
    const receipt = (status) => {
      const value = {
        schema_version: 2, command: "hosted_uninstall_transaction", ok: true, status,
        daemon_admission_fenced: true, attempt_id: state.attempt_id,
        install_path: nativePath(install), helper_path: nativePath(helper),
        binary_sha256: state.binary_sha256, marker_sha256: state.marker_sha256,
      };
      // Deliberately malformed responses exercise the unchanged production parser.
      for (const [key, replacement] of Object.entries(JSON.parse(env.CTX_UNINSTALL_FAKE_RECEIPT_PATCH || "{}"))) {
        if (replacement === null) delete value[key];
        else value[key] = replacement;
      }
      output(value);
    };
    if (prepare) {
      state ??= {
        schema_version: 1, kind: "uninstall", phase: "prepared", attempt_id: args[6],
        install_path: nativePath(install), helper_path: nativePath(helper),
        binary_sha256: hash(install), marker_sha256: hash(marker),
      };
      assert.equal(state.phase, "prepared");
      verify(install, state.binary_sha256);
      verify(marker, state.marker_sha256);
      if (!fs.existsSync(helper)) {
        fs.copyFileSync(install, helper, fs.constants.COPYFILE_EXCL);
        fs.chmodSync(helper, 0o700);
      }
      verify(helper, state.binary_sha256);
      save();
      receipt("prepared");
      return 0;
    }
    assert.ok(state);
    verify(helper, state.binary_sha256);
    if (action === "uninstall-arm") {
      assert.equal(state.phase, "prepared");
      verify(install, state.binary_sha256);
      verify(marker, state.marker_sha256);
      state.phase = "armed";
      save();
      receipt("armed");
      return 0;
    }
    if (state.phase === "prepared") return 96;
    for (const [file, digest] of [[install, state.binary_sha256], [marker, state.marker_sha256]]) {
      if (fs.existsSync(file)) verify(file, digest);
    }
    for (const file of [install, marker]) if (fs.existsSync(file)) fs.unlinkSync(file);
    state.phase = "committed";
    save();
    receipt("committed");
    fs.unlinkSync(journal);
    return 0;
  }
  if (args.length === 1 && args[0] === "--version") {
    fs.writeFileSync(1, `ctx ${env.CTX_UNINSTALL_FAKE_VERSION || "0.26.0"}\n`);
    return 0;
  }
  assert.equal(actor, install);
  if (JSON.stringify(args) === JSON.stringify(["pro", "uninstall", "--help"])) {
    fs.writeFileSync(1, "Usage: ctx pro uninstall <--delete-data|--keep-data> --json\n");
    return 0;
  }
  if (args[2] === "daemon") {
    assert.deepEqual(args, ["--data-root", env.CTX_UNINSTALL_FAKE_DATA_ROOT,
      "daemon", "disable", "--prepare-uninstall", "--format=json"]);
    const status = Number(env.CTX_UNINSTALL_FAKE_DAEMON_STATUS);
    assert.ok(Number.isInteger(status) && status >= 0 && status <= 255);
    if (status) return status;
    if (env.CTX_UNINSTALL_FAKE_DAEMON_REMOVE_STATE === "true") {
      for (const file of daemonFiles) if (fs.existsSync(file)) fs.unlinkSync(file);
    }
    fs.writeFileSync(1, `${env.CTX_UNINSTALL_FAKE_DAEMON_RESULT}\n`);
    return 0;
  }
  assert.ok(["--delete-data", "--keep-data"].includes(args[4]));
  assert.deepEqual(args, ["--data-root", env.CTX_UNINSTALL_FAKE_DATA_ROOT,
    "pro", "uninstall", args[4], "--json"]);
  if (daemonFiles.some((file) => fs.existsSync(file))) return 96;
  output({ uninstalled: true, canonical_history_preserved: true,
    local_pro_data: args[4] === "--delete-data" ? "deleted" : "preserved" });
  return 0;
}

try { process.exitCode = main(); }
catch {
  process.stderr.write("invalid hosted uninstall fixture request\n");
  process.exitCode = 91;
}
