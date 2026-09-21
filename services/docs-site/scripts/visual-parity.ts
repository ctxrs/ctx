import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import pixelmatch from 'pixelmatch';
import { chromium } from 'playwright';
import { PNG } from 'pngjs';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(scriptDir, '..');
const artifactsRoot = path.resolve(projectRoot, '.artifacts', 'visual-parity');

const baselineUrl = process.env.VISUAL_BASELINE_URL ?? 'https://ctx.rs';
const candidateUrl = process.env.VISUAL_CANDIDATE_URL ?? 'http://127.0.0.1:4173';
const routes = [
  '/',
  '/getting-started/install',
  '/search',
  '/providers/native-imports',
  '/reference/cli',
];
const widths = [390, 768, 1280, 1440];
const height = 1600;
const themes = (process.env.VISUAL_THEMES ?? 'dark')
  .split(',')
  .map((value: string) => value.trim())
  .filter((value: string): value is 'dark' | 'light' => value === 'dark' || value === 'light');
const failureThreshold = Number(process.env.VISUAL_FAILURE_THRESHOLD ?? '0.01');
const navigationTimeoutMs = Number(process.env.VISUAL_NAV_TIMEOUT_MS ?? '60000');

function slugifyRoute(route: string): string {
  return route === '/' ? 'home' : route.replace(/^\//, '').replace(/[\/]+/g, '__');
}

async function ensureDir(filePath: string): Promise<void> {
  await fs.mkdir(path.dirname(filePath), { recursive: true });
}

async function captureScreenshot(
  baseUrl: string,
  route: string,
  width: number,
  theme: 'dark' | 'light',
  outputPath: string,
): Promise<void> {
  const browser = await chromium.launch();
  const context = await browser.newContext({
    colorScheme: theme,
    viewport: { height, width },
  });

  await context.addInitScript((selectedTheme) => {
    const keys = ['ctx-docs-theme', 'theme', 'color-theme'];
    for (const key of keys) {
      try {
        window.localStorage.setItem(key, selectedTheme);
      } catch {
        // Ignore storage failures.
      }
    }
  }, theme);

  const page = await context.newPage();
  await page.goto(new URL(route, baseUrl).toString(), {
    timeout: navigationTimeoutMs,
    waitUntil: 'load',
  });
  await page.evaluate(async () => {
    if ('fonts' in document) {
      await document.fonts.ready;
    }
  });
  await page.waitForTimeout(1200);
  await page.addStyleTag({
    content:
      '*,:before,:after{animation:none !important;transition:none !important;caret-color:transparent !important;}',
  });
  await ensureDir(outputPath);
  await page.screenshot({ path: outputPath, fullPage: false });
  await browser.close();
}

async function diffImages(baselinePath: string, candidatePath: string, diffPath: string): Promise<number> {
  const [baselineBuffer, candidateBuffer] = await Promise.all([
    fs.readFile(baselinePath),
    fs.readFile(candidatePath),
  ]);
  const baselineImage = PNG.sync.read(baselineBuffer);
  const candidateImage = PNG.sync.read(candidateBuffer);

  if (baselineImage.width !== candidateImage.width || baselineImage.height !== candidateImage.height) {
    throw new Error(`Mismatched image sizes for ${baselinePath} vs ${candidatePath}`);
  }

  const diffImage = new PNG({ height: baselineImage.height, width: baselineImage.width });
  const diffPixels = pixelmatch(
    baselineImage.data,
    candidateImage.data,
    diffImage.data,
    baselineImage.width,
    baselineImage.height,
    { threshold: 0.1 },
  );

  await ensureDir(diffPath);
  await fs.writeFile(diffPath, PNG.sync.write(diffImage));
  return diffPixels / (baselineImage.width * baselineImage.height);
}

async function main(): Promise<void> {
  const failures: Array<{ ratio: number; route: string; theme: string; width: number }> = [];

  for (const theme of themes) {
    for (const width of widths) {
      for (const route of routes) {
        const slug = slugifyRoute(route);
        const baselinePath = path.join(artifactsRoot, 'baseline', theme, `${slug}-${width}.png`);
        const candidatePath = path.join(artifactsRoot, 'candidate', theme, `${slug}-${width}.png`);
        const diffPath = path.join(artifactsRoot, 'diff', theme, `${slug}-${width}.png`);

        await captureScreenshot(baselineUrl, route, width, theme, baselinePath);
        await captureScreenshot(candidateUrl, route, width, theme, candidatePath);
        const diffRatio = await diffImages(baselinePath, candidatePath, diffPath);

        process.stdout.write(
          `${theme} ${width}px ${route} diff=${(diffRatio * 100).toFixed(2)}%\n`,
        );

        if (diffRatio > failureThreshold) {
          failures.push({ ratio: diffRatio, route, theme, width });
        }
      }
    }
  }

  if (failures.length > 0) {
    const summary = failures
      .map((failure) => `${failure.theme} ${failure.width}px ${failure.route} ${(failure.ratio * 100).toFixed(2)}%`)
      .join('\n');
    throw new Error(`Visual parity failures exceeded threshold ${failureThreshold}:\n${summary}`);
  }
}

void main();
