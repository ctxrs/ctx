import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createServer } from "node:net";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const LOCAL_STDOUT_CAP_BYTES = 64 * 1024 * 1024;
const LOCAL_STDERR_CAP_BYTES = 256 * 1024;
const LOCAL_TERMINATION_GRACE_MS = 750;
const LOCAL_TEARDOWN_MS = 1_000;
const LOCAL_POLL_INTERVAL_MS = 10;
const INITIAL_CAPTURE_BYTES = 64 * 1024;
const WINDOWS_JOB_LAUNCHER = fileURLToPath(
  new URL("./windows-job-launcher.ps1", import.meta.url),
);

export async function spawnCommand(command, args, options, errorTypes) {
  let launch;
  try {
    launch =
      process.platform === "win32"
        ? await prepareWindowsLaunch(command, args, options)
        : directLaunch(command, args, options);
  } catch (cause) {
    throw startupError(command, args, errorTypes.CtxCliError, cause);
  }
  return runCommand(command, args, options, errorTypes, launch);
}

function runCommand(command, args, options, errorTypes, launch) {
  const { CtxCliError, CtxParseError, CtxTimeoutError } = errorTypes;
  return new Promise((resolve, reject) => {
    let child;
    try {
      child = spawn(launch.command, launch.args, launch.options);
    } catch (cause) {
      launch.dispose();
      reject(startupError(command, args, CtxCliError, cause));
      return;
    }

    const stdoutCapture = new BoundedCapture(LOCAL_STDOUT_CAP_BYTES);
    const stderrCapture = new BoundedCapture(LOCAL_STDERR_CAP_BYTES);
    let stdoutEnded = false;
    let stderrEnded = false;
    let processExitObserved = false;
    let processExitCode = null;
    let processSignal = null;
    let targetExitObserved = !launch.control;
    let targetExitCode = null;
    let targetSignal = null;
    let settling = false;
    let termination;
    let captureOverflow;
    let launcherStatusTimer;

    const timeoutMs = Math.max(0, Number(options.timeoutMs));
    const deadline = performance.now() + timeoutMs;
    const deadlineTimer = setTimeout(() => {
      fail(
        () =>
          new CtxTimeoutError(`ctx command timed out after ${options.timeoutMs}ms`, {
            details: {
              command,
              args,
              exitCode: targetExitObserved ? targetExitCode : child.exitCode,
              signal: targetExitObserved ? targetSignal : child.signalCode,
              stdout: stdoutCapture.text(),
              stderr: stderrCapture.text(),
              timeoutMs: options.timeoutMs,
            },
          }),
      );
    }, remainingMilliseconds(deadline));

    const terminate = () => {
      termination ??= terminateProcessTree(child);
      return termination;
    };

    const finish = (settle) => {
      clearTimeout(deadlineTimer);
      clearTimeout(launcherStatusTimer);
      launch.dispose();
      void terminate().finally(() => {
        child.stdin?.destroy();
        child.stdout?.destroy();
        child.stderr?.destroy();
        settle();
      });
    };

    function fail(errorFactory) {
      if (settling) return;
      settling = true;
      finish(() => reject(typeof errorFactory === "function" ? errorFactory() : errorFactory));
    }

    const drain = (stream, chunk) => {
      // Keep both streams flowing through teardown. Once an outcome is fixed, bytes are
      // deliberately discarded instead of removing listeners and risking a blocked owner.
      if (settling) return;
      const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      const capture = stream === "stdout" ? stdoutCapture : stderrCapture;
      if (!capture.append(bytes)) {
        captureOverflow ??= new CtxParseError("ctx CLI output exceeded its capture limit", {
          code: "capture_limit",
          details: { command, args, stream, capBytes: capture.capBytes },
        });
      }
    };

    const maybeResolve = () => {
      if (
        settling ||
        !processExitObserved ||
        !targetExitObserved ||
        !stdoutEnded ||
        !stderrEnded
      ) {
        return;
      }
      settling = true;
      const result = {
        command,
        args,
        exitCode: launch.control ? targetExitCode : processExitCode,
        signal: launch.control ? targetSignal : processSignal,
        stdout: stdoutCapture.buffer(),
        stderr: stderrCapture.buffer(),
      };
      finish(() => {
        if (captureOverflow) reject(captureOverflow);
        else resolve(result);
      });
    };

    if (launch.control) {
      launch.control.onExit((exitCode) => {
        clearTimeout(launcherStatusTimer);
        targetExitObserved = true;
        targetExitCode = exitCode;
        targetSignal = null;
        maybeResolve();
      });
      launch.control.onError((cause) => {
        fail(() => startupError(command, args, CtxCliError, cause));
      });
    }

    child.stdout.on("data", (chunk) => drain("stdout", chunk));
    child.stderr.on("data", (chunk) => drain("stderr", chunk));
    child.stdout.on("end", () => {
      stdoutEnded = true;
      maybeResolve();
    });
    child.stderr.on("end", () => {
      stderrEnded = true;
      maybeResolve();
    });
    child.stdout.on("error", (cause) => {
      fail(
        new CtxParseError("ctx CLI stdout capture failed", {
          code: "capture_failure",
          details: { command, args, stream: "stdout" },
          cause,
        }),
      );
    });
    child.stderr.on("error", (cause) => {
      fail(
        new CtxParseError("ctx CLI stderr capture failed", {
          code: "capture_failure",
          details: { command, args, stream: "stderr" },
          cause,
        }),
      );
    });
    child.on("error", (cause) => {
      fail(() => startupError(command, args, CtxCliError, cause));
    });
    child.on("exit", (code, exitSignal) => {
      processExitObserved = true;
      processExitCode = code;
      processSignal = exitSignal;
      if (!launch.control) {
        targetExitObserved = true;
        targetExitCode = code;
        targetSignal = exitSignal;
        void terminate();
      } else if (!targetExitObserved) {
        // Named-pipe data and process-exit notifications use independent Windows handles.
        // Allow the already-flushed terminal control line to win their delivery race.
        launcherStatusTimer = setTimeout(() => {
          if (settling || targetExitObserved) return;
          fail(
            () =>
              startupError(
                command,
                args,
                CtxCliError,
                new Error(`Windows job launcher exited before reporting target status (${code})`),
              ),
          );
        }, 100);
      }
      maybeResolve();
    });

    if (launch.input) {
      child.stdin.end(launch.input, (cause) => {
        if (cause) fail(() => startupError(command, args, CtxCliError, cause));
      });
    }
  });
}

function startupError(command, args, CtxCliError, cause) {
  return new CtxCliError(`failed to start ${command}`, {
    command,
    args,
    exitCode: undefined,
    stdout: "",
    stderr: "",
    cause,
  });
}

function directLaunch(command, args, options) {
  return {
    command,
    args,
    options: {
      cwd: options.cwd,
      env: options.env,
      stdio: ["ignore", "pipe", "pipe"],
      detached: true,
      windowsHide: true,
    },
    input: undefined,
    control: undefined,
    dispose() {},
  };
}

async function prepareWindowsLaunch(command, args, options) {
  const control = await createWindowsControl();
  const environment = windowsEnvironmentEntries(options.env);
  const payload = Buffer.from(
    JSON.stringify({
      command,
      arguments: args,
      environment,
      currentDirectory: options.cwd ?? "",
    }),
    "utf8",
  ).toString("base64");
  return {
    command: windowsPowerShellPath(),
    args: [
      "-NoLogo",
      "-NoProfile",
      "-NonInteractive",
      "-ExecutionPolicy",
      "Bypass",
      "-File",
      WINDOWS_JOB_LAUNCHER,
      "-ControlPipe",
      control.name,
    ],
    options: {
      cwd: options.cwd,
      env: options.env,
      stdio: ["pipe", "pipe", "pipe"],
      detached: false,
      windowsHide: true,
    },
    input: payload,
    control,
    dispose: () => control.dispose(),
  };
}

function windowsPowerShellPath() {
  const systemRoot = environmentValue(process.env, "SystemRoot");
  return systemRoot
    ? join(systemRoot, "System32", "WindowsPowerShell", "v1.0", "powershell.exe")
    : "powershell.exe";
}

function windowsEnvironmentEntries(environment) {
  const entries = [];
  const seen = new Set();
  for (const key of Object.keys(environment ?? {}).sort()) {
    const folded = key.toLowerCase();
    const value = environment[key];
    if (seen.has(folded) || value === undefined) continue;
    seen.add(folded);
    entries.push({ key, value: String(value) });
  }
  return entries;
}

function environmentValue(environment, wantedKey) {
  const entry = Object.entries(environment).find(
    ([key, value]) => key.toLowerCase() === wantedKey.toLowerCase() && value !== undefined,
  );
  return entry?.[1];
}

async function createWindowsControl() {
  const name = `ctx-agent-history-${process.pid}-${randomBytes(16).toString("hex")}`;
  const path = `\\\\.\\pipe\\${name}`;
  const exitListeners = [];
  const errorListeners = [];
  let targetExitCode;
  let failure;
  let disposed = false;
  let connection;
  let pending = "";

  const server = createServer((socket) => {
    if (connection) {
      socket.destroy();
      return;
    }
    connection = socket;
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {
      pending += chunk;
      for (;;) {
        const newline = pending.indexOf("\n");
        if (newline < 0) break;
        const line = pending.slice(0, newline).replace(/\r$/, "");
        pending = pending.slice(newline + 1);
        consumeWindowsControlLine(line);
      }
    });
    socket.on("error", reportError);
  });
  server.on("error", reportError);

  await new Promise((resolve, reject) => {
    const onError = (cause) => {
      server.off("listening", onListening);
      reject(cause);
    };
    const onListening = () => {
      server.off("error", onError);
      resolve();
    };
    server.once("error", onError);
    server.once("listening", onListening);
    server.listen(path);
  });

  function consumeWindowsControlLine(line) {
    if (line.startsWith("started:")) return;
    if (line.startsWith("exit:")) {
      const code = Number(line.slice("exit:".length));
      if (!Number.isInteger(code) || code < 0 || code > 0xffffffff) {
        reportError(new Error("Windows job launcher reported an invalid exit code"));
        return;
      }
      targetExitCode = code;
      for (const listener of exitListeners) listener(code);
      return;
    }
    if (line.startsWith("error:")) {
      let message;
      try {
        message = Buffer.from(line.slice("error:".length), "base64").toString("utf8");
      } catch {
        message = "Windows job launcher reported an undecodable error";
      }
      reportError(new Error(message));
      return;
    }
    reportError(new Error("Windows job launcher reported an invalid control message"));
  }

  function reportError(cause) {
    if (disposed || failure) return;
    failure = cause;
    for (const listener of errorListeners) listener(cause);
  }

  return {
    name,
    onExit(listener) {
      exitListeners.push(listener);
      if (targetExitCode !== undefined) listener(targetExitCode);
    },
    onError(listener) {
      errorListeners.push(listener);
      if (failure) listener(failure);
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      connection?.destroy();
      server.close();
    },
  };
}

class BoundedCapture {
  constructor(capBytes) {
    this.capBytes = capBytes;
    this.bytes = 0;
    this.storage = Buffer.allocUnsafe(Math.min(INITIAL_CAPTURE_BYTES, capBytes));
  }

  append(bytes) {
    const retained = Math.min(bytes.length, this.capBytes - this.bytes);
    if (retained > 0) {
      this.#ensureCapacity(this.bytes + retained);
      bytes.copy(this.storage, this.bytes, 0, retained);
      this.bytes += retained;
    }
    return retained === bytes.length;
  }

  buffer() {
    return this.storage.subarray(0, this.bytes);
  }

  text() {
    return this.buffer().toString("utf8");
  }

  #ensureCapacity(required) {
    if (required <= this.storage.length) return;
    let capacity = this.storage.length;
    while (capacity < required) capacity = Math.min(this.capBytes, capacity * 2);
    const expanded = Buffer.allocUnsafe(capacity);
    this.storage.copy(expanded, 0, 0, this.bytes);
    this.storage = expanded;
  }
}

function remainingMilliseconds(deadline) {
  return Math.max(0, deadline - performance.now());
}

async function terminateProcessTree(child) {
  const pid = child.pid;
  if (!pid) return;

  if (process.platform === "win32") {
    // The PowerShell launcher is the sole holder of a kill-on-close Job Object. Terminating
    // that holder is identity-safe even after the target root PID and ancestry are gone.
    if (!childHasExited(child)) {
      try {
        child.kill("SIGKILL");
      } catch {}
    }
  } else {
    signalProcessGroup(pid, "SIGTERM");
    await waitForProcessGroupExit(pid, LOCAL_TERMINATION_GRACE_MS);
    signalProcessGroup(pid, "SIGKILL");
    if (!childHasExited(child)) {
      try {
        child.kill("SIGKILL");
      } catch {}
    }
  }
  await waitForChildExit(child, LOCAL_TEARDOWN_MS);
}

function signalProcessGroup(pid, signal) {
  try {
    process.kill(-pid, signal);
  } catch {}
}

async function waitForProcessGroupExit(pid, timeoutMs) {
  await waitUntil(() => !processGroupExists(pid), timeoutMs);
}

function processGroupExists(pid) {
  try {
    process.kill(-pid, 0);
    return true;
  } catch (error) {
    return error?.code === "EPERM";
  }
}

function childHasExited(child) {
  return child.exitCode !== null || child.signalCode !== null;
}

async function waitForChildExit(child, timeoutMs) {
  if (childHasExited(child)) return;
  await Promise.race([
    new Promise((resolve) => child.once("exit", resolve)),
    delay(timeoutMs),
  ]);
}

async function waitUntil(predicate, timeoutMs) {
  const deadline = performance.now() + timeoutMs;
  while (!predicate() && performance.now() < deadline) {
    await delay(Math.min(LOCAL_POLL_INTERVAL_MS, remainingMilliseconds(deadline)));
  }
}

function delay(timeoutMs) {
  return new Promise((resolve) => setTimeout(resolve, timeoutMs));
}
