import type { Page, Request, Response } from "@playwright/test";
import { promises as fs } from "node:fs";
import path from "node:path";
import { performance } from "node:perf_hooks";
import pixelmatch from "pixelmatch";
import { PNG } from "pngjs";

export type MeaningfulPaintFrame = {
  index: number;
  path: string;
  timestampMs: number;
  diffRatio: number;
};

export type NetworkEventEntry =
  | {
      type: "request";
      id: string;
      url: string;
      method: string;
      resourceType?: string;
      timestampMs: number;
    }
  | {
      type: "response";
      id: string;
      url: string;
      status: number;
      ok: boolean;
      resourceType?: string;
      timestampMs: number;
    }
  | {
      type: "requestfailed";
      id: string;
      url: string;
      method: string;
      failureText?: string;
      resourceType?: string;
      timestampMs: number;
    };

export type RecordMeaningfulPaintShiftsOptions = {
  dir: string;
  pollMs?: number;
  stableForMs?: number;
  timeoutMs?: number;
  diffPixelRatio?: number;
  pixelmatchThreshold?: number;
};

export type RecordMeaningfulPaintShiftsResult = {
  frames: MeaningfulPaintFrame[];
  network: NetworkEventEntry[];
};

const DEFAULT_OPTIONS = {
  pollMs: 80,
  stableForMs: 700,
  timeoutMs: 15_000,
  diffPixelRatio: 0.008,
  pixelmatchThreshold: 0.1,
} satisfies Required<Omit<RecordMeaningfulPaintShiftsOptions, "dir">>;

/**
 * Capture "meaningful" visual deltas while a Playwright action runs and return a timestamped trace.
 */
export async function recordMeaningfulPaintShifts(
  page: Page,
  action: () => Promise<void>,
  options: RecordMeaningfulPaintShiftsOptions,
): Promise<RecordMeaningfulPaintShiftsResult> {
  const recorder = new MeaningfulPaintRecorder(page, options);
  await recorder.start();
  await action();
  return recorder.stop();
}

class MeaningfulPaintRecorder {
  private readonly page: Page;
  private readonly dir: string;
  private readonly pollMs: number;
  private readonly stableForMs: number;
  private readonly timeoutMs: number;
  private readonly diffPixelRatio: number;
  private readonly pixelmatchThreshold: number;
  private readonly runStartedAt = performance.now();
  private readonly networkLog: NetworkEventEntry[] = [];
  private readonly requestIds = new Map<Request, string>();
  private requestCounter = 0;
  private capturePromise: Promise<MeaningfulPaintFrame[]> | null = null;
  private stopRequested = false;
  private listenersInstalled = false;

  constructor(page: Page, opts: RecordMeaningfulPaintShiftsOptions) {
    this.page = page;
    this.dir = opts.dir;
    this.pollMs = opts.pollMs ?? DEFAULT_OPTIONS.pollMs;
    this.stableForMs = opts.stableForMs ?? DEFAULT_OPTIONS.stableForMs;
    this.timeoutMs = opts.timeoutMs ?? DEFAULT_OPTIONS.timeoutMs;
    this.diffPixelRatio = opts.diffPixelRatio ?? DEFAULT_OPTIONS.diffPixelRatio;
    this.pixelmatchThreshold = opts.pixelmatchThreshold ?? DEFAULT_OPTIONS.pixelmatchThreshold;
  }

  async start(): Promise<void> {
    await fs.mkdir(this.dir, { recursive: true });
    this.attachNetworkListeners();
    this.capturePromise = this.captureLoop();
  }

  async stop(): Promise<RecordMeaningfulPaintShiftsResult> {
    this.stopRequested = true;
    const frames = (await this.capturePromise) ?? [];
    this.detachNetworkListeners();
    return {
      frames,
      network: this.networkLog,
    };
  }

  private attachNetworkListeners() {
    if (this.listenersInstalled) return;
    this.listenersInstalled = true;
    this.page.on("request", this.onRequest);
    this.page.on("response", this.onResponse);
    this.page.on("requestfailed", this.onRequestFailed);
  }

  private detachNetworkListeners() {
    if (!this.listenersInstalled) return;
    this.page.off("request", this.onRequest);
    this.page.off("response", this.onResponse);
    this.page.off("requestfailed", this.onRequestFailed);
    this.listenersInstalled = false;
    this.requestIds.clear();
  }

  private onRequest = (request: Request) => {
    const id = this.nextRequestId();
    this.requestIds.set(request, id);
    this.networkLog.push({
      type: "request",
      id,
      method: request.method(),
      url: request.url(),
      resourceType: request.resourceType(),
      timestampMs: this.elapsed(),
    });
  };

  private onResponse = (response: Response) => {
    const request = response.request();
    const id = this.requestIds.get(request) ?? this.nextRequestId();
    this.networkLog.push({
      type: "response",
      id,
      url: response.url(),
      status: response.status(),
      ok: response.ok(),
      resourceType: request.resourceType(),
      timestampMs: this.elapsed(),
    });
    this.requestIds.delete(request);
  };

  private onRequestFailed = (request: Request) => {
    const id = this.requestIds.get(request) ?? this.nextRequestId();
    this.networkLog.push({
      type: "requestfailed",
      id,
      url: request.url(),
      method: request.method(),
      failureText: request.failure()?.errorText,
      resourceType: request.resourceType(),
      timestampMs: this.elapsed(),
    });
    this.requestIds.delete(request);
  };

  private async captureLoop(): Promise<MeaningfulPaintFrame[]> {
    const frames: MeaningfulPaintFrame[] = [];
    let lastAccepted = await this.waitForFirstShot();
    if (!lastAccepted) return frames;

    let stableSince = performance.now();
    frames.push(await this.saveFrame(lastAccepted, 0));

    while (performance.now() - this.runStartedAt < this.timeoutMs) {
      await this.page.waitForTimeout(this.pollMs);
      const current = await this.safeScreenshot();
      if (!current) continue;

      const ratio = this.diffRatio(lastAccepted, current);
      if (ratio >= this.diffPixelRatio) {
        lastAccepted = current;
        stableSince = performance.now();
        frames.push(await this.saveFrame(current, ratio));
      } else if (this.stopRequested && performance.now() - stableSince >= this.stableForMs) {
        break;
      }
    }

    return frames;
  }

  private async waitForFirstShot(): Promise<Buffer | null> {
    const firstShotDeadline = performance.now() + this.timeoutMs;
    while (performance.now() < firstShotDeadline) {
      const shot = await this.safeScreenshot();
      if (shot) return shot;
      await this.page.waitForTimeout(Math.min(this.pollMs, 50));
    }
    return null;
  }

  private async safeScreenshot(): Promise<Buffer | null> {
    try {
      return await this.page.screenshot({ type: "png", animations: "allow", caret: "hide" });
    } catch {
      return null;
    }
  }

  private diffRatio(prev: Buffer, next: Buffer): number {
    const img1 = PNG.sync.read(prev);
    const img2 = PNG.sync.read(next);
    if (img1.width !== img2.width || img1.height !== img2.height) {
      return 1;
    }
    const { width, height } = img1;
    const diff = new PNG({ width, height });
    const diffPixels = pixelmatch(img1.data, img2.data, diff.data, width, height, {
      threshold: this.pixelmatchThreshold,
    });
    return diffPixels / (width * height);
  }

  private frameIndex = 0;

  private async saveFrame(buffer: Buffer, diffRatio: number): Promise<MeaningfulPaintFrame> {
    const timestampMs = this.elapsed();
    const index = this.frameIndex++;
    const filename = `${String(index).padStart(3, "0")}-${Math.round(timestampMs)}.png`;
    const filepath = path.join(this.dir, filename);
    await fs.writeFile(filepath, buffer);
    return { index, path: filepath, timestampMs, diffRatio };
  }

  private elapsed(): number {
    return performance.now() - this.runStartedAt;
  }

  private nextRequestId(): string {
    this.requestCounter += 1;
    return `req-${this.requestCounter.toString().padStart(4, "0")}`;
  }
}
