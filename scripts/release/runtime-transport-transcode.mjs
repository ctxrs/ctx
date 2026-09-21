import childProcess from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";

const MAX_RUNTIME_BYTES = 1024 * 1024 * 1024;

function fail(message) { throw new Error(message); }

function writeExclusive(file, body) {
  const descriptor = fs.openSync(file, "wx", 0o600);
  try {
    fs.writeFileSync(descriptor, body);
    fs.fsyncSync(descriptor);
  } finally {
    fs.closeSync(descriptor);
  }
}

export function transcodeRuntimeTarZstd(sourceBody, label = "runtime transport") {
  if (!Buffer.isBuffer(sourceBody) || sourceBody.length < 1
      || sourceBody.length > MAX_RUNTIME_BYTES) {
    fail(`${label} source is not bounded bytes`);
  }
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-runtime-transcode."));
  try {
    const source = path.join(temporary, "runtime.tar.zst");
    const output = path.join(temporary, "runtime.tar.gz");
    writeExclusive(source, sourceBody);
    const program = String.raw`
import gzip
import subprocess
import sys

source, destination, maximum_text = sys.argv[1:]
maximum = int(maximum_text)
with open(destination, "xb") as raw_output:
    with gzip.GzipFile(filename="", mode="wb", fileobj=raw_output, compresslevel=9, mtime=0) as output:
        process = subprocess.Popen(["zstd", "-q", "-d", "-c", source], stdout=subprocess.PIPE)
        if process.stdout is None:
            raise SystemExit("zstd did not provide decompressed runtime bytes")
        total = 0
        with process.stdout:
            while True:
                chunk = process.stdout.read(1024 * 1024)
                if not chunk:
                    break
                total += len(chunk)
                if total > maximum:
                    process.kill()
                    raise SystemExit("decompressed runtime exceeds the size limit")
                output.write(chunk)
        status = process.wait()
        if status != 0:
            raise SystemExit(f"zstd decompression failed with status {status}")
`;
    const environment = {
      HOME: "/var/empty",
      LANG: process.env.LANG ?? "C",
      LC_ALL: process.env.LC_ALL ?? "C",
      PATH: process.env.PATH ?? "/usr/bin:/bin",
      TMPDIR: process.env.TMPDIR ?? os.tmpdir(),
    };
    const result = childProcess.spawnSync(
      "python3",
      ["-c", program, source, output, String(MAX_RUNTIME_BYTES)],
      { encoding: "utf8", env: environment, timeout: 60_000 },
    );
    if (result.error != null || result.status !== 0) {
      fail(`${label} transcode failed: ${(result.stderr ?? "").trim()}`);
    }
    const body = fs.readFileSync(output);
    if (body.length < 1 || body.length > MAX_RUNTIME_BYTES) {
      fail(`${label} transcode output is not bounded bytes`);
    }
    return body;
  } finally {
    fs.rmSync(temporary, { recursive: true });
  }
}
