import { spawn } from "node:child_process";
import { writeFileSync } from "node:fs";

const [mode, pidPath, delayMs] = process.argv.slice(2);
const exactJson = ' \n{"message":"exact 日本語 🦀","nested":[1,true,null]}\n';

function spawnPipeDescendant(ignoreTerm, ready) {
  const signalHandler = ignoreTerm ? "process.on('SIGTERM',()=>{});" : "";
  const script = `${signalHandler}process.send('ready');setInterval(() => {}, 60_000)`;
  const descendant = spawn(process.execPath, ["-e", script], {
    stdio: ["ignore", "inherit", "inherit", "ipc"],
  });
  descendant.once("message", () => {
    descendant.disconnect();
    descendant.unref();
    ready(descendant);
  });
}

function spawnSilentDescendant(ready) {
  const descendant = spawn(
    process.execPath,
    ["-e", "process.send('ready');setInterval(() => {}, 60_000)"],
    { stdio: ["ignore", "ignore", "ignore", "ipc"] },
  );
  descendant.once("message", () => {
    descendant.disconnect();
    descendant.unref();
    ready(descendant);
  });
}

switch (mode) {
  case "exact-json":
    process.stdout.write(exactJson);
    process.stderr.write("exact stderr\n");
    break;
  case "persistent-pipe": {
    spawnPipeDescendant(false, (descendant) => {
      writeFileSync(pidPath, String(descendant.pid));
      process.stdout.write(exactJson);
    });
    break;
  }
  case "persistent-silent": {
    spawnSilentDescendant((descendant) => {
      writeFileSync(pidPath, String(descendant.pid));
      process.stdout.write(exactJson);
    });
    break;
  }
  case "persistent-pipe-ignored-term": {
    spawnPipeDescendant(true, (descendant) => {
      writeFileSync(pidPath, String(descendant.pid));
      setTimeout(() => process.stdout.write(exactJson), Number(delayMs));
    });
    break;
  }
  case "ignored-term": {
    process.on("SIGTERM", () => {});
    spawnPipeDescendant(true, (descendant) => {
      writeFileSync(pidPath, JSON.stringify([process.pid, descendant.pid]));
      process.stdout.write(exactJson);
      setInterval(() => {}, 60_000);
    });
    break;
  }
  case "oversized-stdout":
    process.stdout.write(Buffer.alloc(64 * 1024 * 1024 + 1, 120), () => {
      writeFileSync(pidPath, "completed");
    });
    break;
  case "oversized-stderr":
    process.stderr.write(Buffer.alloc(256 * 1024 + 1, 121), () => {
      writeFileSync(pidPath, "completed");
    });
    break;
  case "large-valid-json":
    process.stdout.write(`${JSON.stringify({ payload: "x".repeat(3 * 1024 * 1024) })}\n`);
    break;
  case "nonzero":
    process.stdout.write('{"partial":true}\n');
    process.stderr.write("bad flag\n");
    process.exitCode = 2;
    break;
  case "malformed":
    process.stdout.write("not json");
    break;
  default:
    process.stderr.write(`unknown fixture mode: ${mode}\n`);
    process.exitCode = 64;
}
