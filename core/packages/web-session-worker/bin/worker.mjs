import http from "http";
import { WebSocketServer } from "ws";
import { spawn } from "child_process";
import fs from "fs/promises";
import { chromium } from "playwright";
import wrtc from "wrtc";
import {
  isWorkerAuthValid,
  readWorkerAuthSecret,
} from "./auth.mjs";

const PORT = Number(process.env.PORT || 0);
const TARGET_URL = process.env.TARGET_URL || "https://www.google.com";
const WIDTH = Number(process.env.WIDTH || 1280);
const HEIGHT = Number(process.env.HEIGHT || 720);
const FPS = Number(process.env.FPS || 30);
const DISPLAY = process.env.DISPLAY || ":99";
const WORK_DIR = process.env.WORK_DIR || process.cwd();
const FFMPEG = process.env.FFMPEG_PATH || "ffmpeg";
const XVFB = process.env.XVFB_PATH || "Xvfb";
const ICE_SERVERS = [{ urls: "stun:stun.l.google.com:19302" }];
const MAP_META_TO_CTRL = process.env.MAP_META_TO_CTRL !== "0";

const { RTCVideoSource } = wrtc.nonstandard;
const WORKER_AUTH_SECRET = await readWorkerAuthSecret();

let browser = null;
let context = null;
let page = null;
let cdp = null;
let xvfbProc = null;
let ffmpegProc = null;
let runLock = Promise.resolve();

let inputTransform = {
  dpr: 1,
  offsetX: 0,
  offsetY: 0,
  innerWidth: WIDTH,
  innerHeight: HEIGHT,
};

function toPageCoords(x, y) {
  const dpr = inputTransform.dpr || 1;
  const xCss = (Number(x) - inputTransform.offsetX) / dpr;
  const yCss = (Number(y) - inputTransform.offsetY) / dpr;
  const xClamped = Math.max(0, Math.min(inputTransform.innerWidth, xCss));
  const yClamped = Math.max(0, Math.min(inputTransform.innerHeight, yCss));
  return { x: xClamped, y: yClamped };
}

const videoSource = new RTCVideoSource();
const videoTrack = videoSource.createTrack();
let viewers = 0;

const server = http.createServer(async (req, res) => {
  if (!isWorkerAuthValid(req.headers, WORKER_AUTH_SECRET)) {
    res.writeHead(401, { "content-type": "text/plain" });
    res.end("unauthorized");
    return;
  }

  if (req.method === "GET" && req.url === "/health") {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ ok: true }));
    return;
  }

  if (req.method === "POST" && (req.url === "/run" || req.url === "/eval")) {
    const chunks = [];
    for await (const chunk of req) {
      chunks.push(chunk);
    }
    const body = Buffer.concat(chunks).toString("utf-8");
    let payload = {};
    try {
      payload = JSON.parse(body || "{}");
    } catch (err) {
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ ok: false, error: "invalid_json" }));
      return;
    }

    const timeoutMs = Number(payload.timeout_ms || 0);
    const code = await resolveCode(payload);
    if (!code) {
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ ok: false, error: "missing_script" }));
      return;
    }

    const isEval = req.url === "/eval";
    try {
      const result = await enqueueRun(() => runCode(code, timeoutMs, isEval));
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ ok: true, result }));
    } catch (err) {
      res.writeHead(500, { "content-type": "application/json" });
      res.end(JSON.stringify({ ok: false, error: String(err?.message || err) }));
    }
    return;
  }

  res.writeHead(404, { "content-type": "text/plain" });
  res.end("not found");
});

const wss = new WebSocketServer({ noServer: true });

server.on("upgrade", (req, socket, head) => {
  if (req.url !== "/signal") {
    socket.destroy();
    return;
  }
  if (!isWorkerAuthValid(req.headers, WORKER_AUTH_SECRET)) {
    socket.destroy();
    return;
  }
  wss.handleUpgrade(req, socket, head, (ws) => {
    wss.emit("connection", ws, req);
  });
});

wss.on("connection", (ws) => {
  viewers += 1;

  const pc = new wrtc.RTCPeerConnection({ iceServers: ICE_SERVERS });
  const mediaStream = new wrtc.MediaStream();
  mediaStream.addTrack(videoTrack);
  pc.addTrack(videoTrack, mediaStream);

  pc.onicecandidate = (ev) => {
    if (ev.candidate) {
      ws.send(JSON.stringify({ type: "candidate", candidate: ev.candidate }));
    }
  };

  ws.on("message", async (data) => {
    let msg;
    try {
      msg = JSON.parse(String(data));
    } catch {
      return;
    }

    if (msg.type === "offer") {
      await pc.setRemoteDescription({ type: "offer", sdp: msg.sdp });
      const answer = await pc.createAnswer();
      await pc.setLocalDescription(answer);
      ws.send(JSON.stringify({ type: "answer", sdp: answer.sdp }));
      return;
    }

    if (msg.type === "candidate") {
      if (msg.candidate) {
        await pc.addIceCandidate(msg.candidate);
      }
      return;
    }

    if (msg.type === "cursor_probe") {
      await handleCursorProbe(ws, msg);
      return;
    }

    handleControlMessage(msg);
  });

  ws.on("close", () => {
    viewers -= 1;
    pc.close();
    if (viewers < 0) viewers = 0;
  });
});

async function resolveCode(payload) {
  if (payload.code && typeof payload.code === "string") {
    return payload.code;
  }
  if (payload.script_path && typeof payload.script_path === "string") {
    const path = payload.script_path;
    return fs.readFile(path, "utf-8");
  }
  return null;
}

async function enqueueRun(fn) {
  let runResolve;
  let runReject;
  const next = new Promise((resolve, reject) => {
    runResolve = resolve;
    runReject = reject;
  });
  const current = runLock.then(async () => {
    try {
      const result = await fn();
      runResolve(result);
    } catch (err) {
      runReject(err);
    }
  });
  runLock = current.catch(() => {}).then(() => next);
  return next;
}

async function runCode(code, timeoutMs, isEval) {
  const runner = async () => {
    const fn = new Function("page", "context", "browser", `"use strict"; return (async () => { ${code} })();`);
    return fn(page, context, browser);
  };

  if (!timeoutMs || timeoutMs <= 0) {
    return runner();
  }
  let timer = null;
  try {
    return await Promise.race([
      runner(),
      new Promise((_, reject) => {
        timer = setTimeout(() => {
          reject(new Error(`timeout after ${timeoutMs}ms`));
        }, timeoutMs);
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

async function handleCursorProbe(ws, msg) {
  try {
    const coords = toPageCoords(msg.x, msg.y);
    const cursor = await page.evaluate(({ x, y }) => {
      const el = document.elementFromPoint(x, y);
      if (!el) return "default";
      const style = window.getComputedStyle(el).cursor;
      return style || "default";
    }, coords);
    ws.send(JSON.stringify({ type: "cursor", cursor }));
  } catch {
    ws.send(JSON.stringify({ type: "cursor", cursor: "default" }));
  }
}

function handleControlMessage(msg) {
  if (!msg || typeof msg !== "object") return;
  if (!cdp) return;

  if (msg.type === "mouse") {
    const { event, button, buttons, clickCount, deltaX, deltaY, modifiers } = msg;
    const { x, y } = toPageCoords(msg.x, msg.y);
    if (event === "move") {
      return cdp.send("Input.dispatchMouseEvent", {
        type: "mouseMoved",
        x,
        y,
        modifiers: modifiers ?? 0,
        buttons: buttons ?? 0,
      });
    }
    if (event === "down") {
      return cdp.send("Input.dispatchMouseEvent", {
        type: "mousePressed",
        x,
        y,
        button: button ?? "left",
        modifiers: modifiers ?? 0,
        buttons: buttons ?? 0,
        clickCount: clickCount ?? 1,
      });
    }
    if (event === "up") {
      return cdp.send("Input.dispatchMouseEvent", {
        type: "mouseReleased",
        x,
        y,
        button: button ?? "left",
        modifiers: modifiers ?? 0,
        buttons: buttons ?? 0,
        clickCount: clickCount ?? 1,
      });
    }
    if (event === "wheel") {
      return cdp.send("Input.dispatchMouseEvent", {
        type: "mouseWheel",
        x,
        y,
        modifiers: modifiers ?? 0,
        deltaX: deltaX ?? 0,
        deltaY: deltaY ?? 0,
      });
    }
  }

  if (msg.type === "key") {
    let { event, key, code, keyCode, text, modifiers, raw } = msg;
    if (MAP_META_TO_CTRL && typeof modifiers === "number" && (modifiers & 4) && !(modifiers & 2)) {
      modifiers = (modifiers & ~4) | 2;
      if (key === "Meta") {
        key = "Control";
        code = "ControlLeft";
        keyCode = 17;
      }
    }
    const base = {
      type: event === "up" ? "keyUp" : (raw ? "rawKeyDown" : "keyDown"),
      key,
      code,
      modifiers: modifiers ?? 0,
      windowsVirtualKeyCode: keyCode ?? 0,
      nativeVirtualKeyCode: keyCode ?? 0,
    };
    if (event !== "up" && text) {
      base.text = text;
      base.unmodifiedText = text;
    }
    return cdp.send("Input.dispatchKeyEvent", base);
  }
}

async function startXvfb() {
  xvfbProc = spawn(XVFB, [DISPLAY, "-screen", "0", `${WIDTH}x${HEIGHT}x24`, "-nolisten", "tcp"], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  xvfbProc.stderr.on("data", (d) => console.log(`[xvfb] ${String(d).trim()}`));
  await new Promise((resolve) => setTimeout(resolve, 500));
}

async function startBrowser() {
  browser = await chromium.launch({
    headless: false,
    args: [
      "--disable-dev-shm-usage",
      "--no-sandbox",
      "--disable-gpu",
      `--window-size=${WIDTH},${HEIGHT}`,
      "--window-position=0,0",
      "--force-device-scale-factor=1",
      "--start-fullscreen",
      "--kiosk",
    ],
    env: { ...process.env, DISPLAY },
  });
  context = await browser.newContext({ viewport: null, ignoreHTTPSErrors: true });
  page = await context.newPage();
  page.on("console", (msg) => console.log(`[console:${msg.type()}] ${msg.text()}`));
  page.on("pageerror", (err) => console.log(`[pageerror] ${err.message}`));
  await page.goto(TARGET_URL, { waitUntil: "domcontentloaded" });
  await page.waitForTimeout(1500);
  cdp = await context.newCDPSession(page);
  await refreshInputTransform();
  console.log(`playwright ready: ${TARGET_URL}`);
}

async function refreshInputTransform() {
  if (!page) return;
  try {
    const metrics = await page.evaluate(() => ({
      innerWidth: window.innerWidth,
      innerHeight: window.innerHeight,
      outerWidth: window.outerWidth,
      outerHeight: window.outerHeight,
      screenX: window.screenX,
      screenY: window.screenY,
      dpr: window.devicePixelRatio || 1,
    }));
    const dpr = metrics.dpr || 1;
    const viewportPhysW = (metrics.innerWidth || WIDTH) * dpr;
    const viewportPhysH = (metrics.innerHeight || HEIGHT) * dpr;
    const chromeX = Math.max(0, (WIDTH - viewportPhysW) / 2);
    const chromeY = Math.max(0, HEIGHT - viewportPhysH);
    const windowOffsetX = (metrics.screenX || 0) * dpr;
    const windowOffsetY = (metrics.screenY || 0) * dpr;
    inputTransform = {
      dpr,
      offsetX: chromeX + windowOffsetX,
      offsetY: chromeY + windowOffsetY,
      innerWidth: metrics.innerWidth || WIDTH,
      innerHeight: metrics.innerHeight || HEIGHT,
    };
    console.log(
      `[input] dpr=${inputTransform.dpr} offset=(${inputTransform.offsetX},${inputTransform.offsetY}) inner=${inputTransform.innerWidth}x${inputTransform.innerHeight}`
    );
  } catch (err) {
    console.log(`[input] metrics failed: ${err.message}`);
  }
}

function startCapture() {
  const frameSize = Math.floor(WIDTH * HEIGHT * 1.5);
  const args = [
    "-f",
    "x11grab",
    "-video_size",
    `${WIDTH}x${HEIGHT}`,
    "-framerate",
    String(FPS),
    "-draw_mouse",
    "0",
    "-i",
    `${DISPLAY}.0`,
    "-an",
    "-pix_fmt",
    "yuv420p",
    "-f",
    "rawvideo",
    "-",
  ];
  ffmpegProc = spawn(FFMPEG, args, { stdio: ["ignore", "pipe", "pipe"] });
  ffmpegProc.stderr.on("data", (d) => console.log(`[ffmpeg] ${String(d).trim()}`));

  let buffer = Buffer.alloc(0);
  ffmpegProc.stdout.on("data", (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    while (buffer.length >= frameSize) {
      const frame = buffer.subarray(0, frameSize);
      buffer = buffer.subarray(frameSize);
      videoSource.onFrame({ width: WIDTH, height: HEIGHT, data: new Uint8ClampedArray(frame) });
    }
  });
}

async function main() {
  if (!PORT) {
    throw new Error("PORT is required");
  }
  await fs.mkdir(WORK_DIR, { recursive: true });
  await startXvfb();
  await startBrowser();
  startCapture();
  server.listen(PORT, "127.0.0.1", () => {
    console.log(`web-session-worker listening on 127.0.0.1:${PORT}`);
  });
}

async function shutdown() {
  try {
    if (ffmpegProc) ffmpegProc.kill("SIGKILL");
  } catch {}
  try {
    if (xvfbProc) xvfbProc.kill("SIGKILL");
  } catch {}
  try {
    if (browser) await browser.close();
  } catch {}
  process.exit(0);
}

process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
