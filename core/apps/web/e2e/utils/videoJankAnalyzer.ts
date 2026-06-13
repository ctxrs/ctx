import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { PNG } from "pngjs";

type ShiftRow = {
  from: string;
  to: string;
  index: number;
  timeSec: number;
  shift: number;
  score: number;
};

type AnalyzeOptions = {
  fps?: number;
  scaleWidth?: number;
  maxShiftPx?: number;
  thresholdPx?: number;
  jankThresholdPx?: number;
  skipFrames?: number;
  minBaselineAbsPx?: number;
  startSec?: number;
  durationSec?: number;
  outputDir: string;
  maxDiffFrames?: number;
};

const loadPng = (filePath: string) => PNG.sync.read(fs.readFileSync(filePath));

const toGray = (png: PNG) => {
  const { width, height, data } = png;
  const gray = new Uint8Array(width * height);
  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const i = (y * width + x) * 4;
      const r = data[i];
      const g = data[i + 1];
      const b = data[i + 2];
      gray[y * width + x] = (0.2126 * r + 0.7152 * g + 0.0722 * b) | 0;
    }
  }
  return gray;
};

const measureShift = (
  aGray: Uint8Array,
  bGray: Uint8Array,
  width: number,
  height: number,
  maxShift: number,
) => {
  const x0 = Math.floor(width * 0.2);
  const x1 = Math.floor(width * 0.8);
  const y0 = Math.floor(height * 0.2);
  const y1 = Math.floor(height * 0.9);

  let bestShift = 0;
  let bestScore = Infinity;

  for (let dy = -maxShift; dy <= maxShift; dy += 1) {
    let sum = 0;
    let count = 0;
    for (let y = y0; y < y1; y += 1) {
      const yb = y + dy;
      if (yb < y0 || yb >= y1) continue;
      const rowA = y * width;
      const rowB = yb * width;
      for (let x = x0; x < x1; x += 1) {
        const va = aGray[rowA + x];
        const vb = bGray[rowB + x];
        sum += Math.abs(va - vb);
        count += 1;
      }
    }
    const score = sum / Math.max(count, 1);
    if (score < bestScore) {
      bestScore = score;
      bestShift = dy;
    }
  }

  return { bestShift, bestScore };
};

export const canUseFfmpeg = (): boolean => {
  try {
    execFileSync("ffmpeg", ["-version"], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
};

export const analyzeScrollJankVideo = (videoPath: string, opts: AnalyzeOptions) => {
  const fps = opts.fps ?? 10;
  const scaleWidth = opts.scaleWidth ?? 960;
  const maxShiftPx = opts.maxShiftPx ?? 40;
  const thresholdPx = opts.thresholdPx ?? 20;
  const jankThresholdPx = opts.jankThresholdPx ?? thresholdPx;
  const skipFrames = opts.skipFrames ?? 0;
  const minBaselineAbsPx = opts.minBaselineAbsPx ?? 1;
  const maxDiffFrames = opts.maxDiffFrames ?? 3;

  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-jank-frames-"));
  const outputDir = opts.outputDir;
  fs.mkdirSync(outputDir, { recursive: true });
  const framePattern = path.join(tempDir, "frame_%03d.png");

  const ffmpegArgs = ["-y"];
  if (opts.startSec != null) ffmpegArgs.push("-ss", String(opts.startSec));
  ffmpegArgs.push("-i", videoPath);
  if (opts.durationSec != null) ffmpegArgs.push("-t", String(opts.durationSec));
  ffmpegArgs.push("-vf", `fps=${fps},scale=${scaleWidth}:-1`);
  ffmpegArgs.push(framePattern);

  execFileSync("ffmpeg", ffmpegArgs, { stdio: "ignore" });

  const frames = fs
    .readdirSync(tempDir)
    .filter((name) => name.startsWith("frame_") && name.endsWith(".png"))
    .sort();

  if (frames.length < 2) {
    throw new Error("Not enough frames extracted for jitter analysis.");
  }

  const rows: ShiftRow[] = [];
  let prevPng = loadPng(path.join(tempDir, frames[0]));
  let prevGray = toGray(prevPng);

  for (let i = 1; i < frames.length; i += 1) {
    const currentPng = loadPng(path.join(tempDir, frames[i]));
    const currentGray = toGray(currentPng);
    const { bestShift, bestScore } = measureShift(
      prevGray,
      currentGray,
      currentPng.width,
      currentPng.height,
      maxShiftPx,
    );
    rows.push({
      from: frames[i - 1],
      to: frames[i],
      index: i,
      timeSec: Number((i / fps).toFixed(2)),
      shift: bestShift,
      score: Number(bestScore.toFixed(2)),
    });
    prevGray = currentGray;
  }

  const csvPath = path.join(outputDir, "scrollback-jank-shifts.csv");
  const jsonPath = path.join(outputDir, "scrollback-jank-shifts.json");
  const summaryPath = path.join(outputDir, "scrollback-jank-summary.json");

  fs.writeFileSync(
    csvPath,
    ["from,to,index,timeSec,shift,score", ...rows.map((r) => `${r.from},${r.to},${r.index},${r.timeSec},${r.shift},${r.score}`)].join(
      "\n",
    ),
  );
  fs.writeFileSync(jsonPath, JSON.stringify(rows, null, 2));

  const rowsForBaseline = rows.slice(Math.min(skipFrames, rows.length));
  const shifts = rowsForBaseline.map((r) => r.shift);
  const absShifts = shifts.map((v) => Math.abs(v)).sort((a, b) => a - b);
  const medianAbs =
    absShifts.length === 0
      ? 0
      : absShifts.length % 2 === 1
        ? absShifts[(absShifts.length - 1) / 2]
        : (absShifts[absShifts.length / 2 - 1] + absShifts[absShifts.length / 2]) / 2;
  const baselineAbs = Math.max(minBaselineAbsPx, medianAbs);
  const sumShift = shifts.reduce((acc, value) => acc + value, 0);
  const baselineSign = sumShift === 0 ? 0 : Math.sign(sumShift);
  const baselineShift = baselineSign === 0 ? 0 : baselineSign * baselineAbs;

  const spikes = [...rows].sort((a, b) => Math.abs(b.shift) - Math.abs(a.shift));
  const maxAbsShift = spikes.length > 0 ? Math.abs(spikes[0].shift) : 0;
  const flagged = rows.filter((r, idx) => {
    if (idx < skipFrames) return false;
    const delta = Math.abs(r.shift - baselineShift);
    const signFlip = baselineShift !== 0 && Math.sign(r.shift) !== Math.sign(baselineShift);
    return delta >= jankThresholdPx || (signFlip && Math.abs(r.shift) >= baselineAbs + jankThresholdPx);
  });

  const snapbackPairs = [];
  for (let i = 1; i < rows.length; i += 1) {
    const prev = rows[i - 1];
    const current = rows[i];
    if (i - 1 < skipFrames) continue;
    if (Math.abs(prev.shift) < baselineAbs + jankThresholdPx) continue;
    if (Math.abs(current.shift) < baselineAbs + jankThresholdPx) continue;
    if (Math.sign(prev.shift) === Math.sign(current.shift)) continue;
    snapbackPairs.push({ index: i, prev, current });
  }
  const maxJankAbsShift = flagged.length > 0 ? Math.max(...flagged.map((r) => Math.abs(r.shift))) : 0;

  fs.writeFileSync(
    summaryPath,
    JSON.stringify(
      {
        videoPath,
        fps,
        scaleWidth,
        maxShiftPx,
        thresholdPx,
        jankThresholdPx,
        baselineShift,
        baselineAbs,
        maxAbsShift,
        maxJankAbsShift,
        flaggedCount: flagged.length,
        snapbackPairs: snapbackPairs.slice(0, 5),
        topSpikes: spikes.slice(0, 5),
      },
      null,
      2,
    ),
  );

  const diffPaths: string[] = [];
  const diffSource = flagged.length > 0 ? [...flagged].sort((a, b) => Math.abs(b.shift) - Math.abs(a.shift)) : spikes;
  for (const spike of diffSource.slice(0, maxDiffFrames)) {
    const aPath = path.join(tempDir, spike.from);
    const bPath = path.join(tempDir, spike.to);
    const diffPath = path.join(outputDir, `scrollback-jank-diff-${spike.index}.png`);
    execFileSync(
      "ffmpeg",
      [
        "-y",
        "-i",
        aPath,
        "-i",
        bPath,
        "-filter_complex",
        "[0:v][1:v]blend=all_mode=difference,eq=contrast=4:brightness=0.0[diff];[0:v][1:v][diff]hstack=inputs=3",
        "-frames:v",
        "1",
        "-update",
        "1",
        diffPath,
      ],
      { stdio: "ignore" },
    );
    diffPaths.push(diffPath);
  }

  try {
    fs.rmSync(tempDir, { recursive: true, force: true });
  } catch {
    // ignore cleanup errors
  }

  return {
    rows,
    spikes,
    maxAbsShift,
    maxJankAbsShift,
    baselineShift,
    baselineAbs,
    flagged,
    snapbackPairs,
    csvPath,
    jsonPath,
    summaryPath,
    diffPaths,
  };
};
