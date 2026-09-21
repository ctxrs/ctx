import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { toSiteHref } from '../src/lib/route-hrefs';
import { renderRoute } from '../src/render';
import { getSiteData } from '../src/lib/site-data';
import { SITE_ORIGIN } from '../src/lib/site-origin';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(scriptDir, '..');
const distDir = path.resolve(projectRoot, 'dist');
const templatePath = path.resolve(distDir, 'index.html');

function isExternalUrl(value: string): boolean {
  return /^[a-z][a-z0-9+.-]*:/i.test(value) || value.startsWith('//');
}

async function writeRoute(template: string, pathname: string): Promise<void> {
  const rendered = renderRoute(pathname);
  const routeTemplate = template
    .replace('<!--app-head-->', rendered.headHtml)
    .replace('<!--app-html-->', rendered.html);

  const relativeDir = pathname === '/' ? '' : pathname.slice(1);
  const outputDir = path.join(distDir, relativeDir);
  const outputPath = path.join(outputDir || distDir, 'index.html');
  await fs.mkdir(outputDir || distDir, { recursive: true });
  await fs.writeFile(outputPath, routeTemplate, 'utf8');
}

export function renderRedirectPage(target: string): string {
  const targetHref = toSiteHref(target);
  const canonicalHref = isExternalUrl(targetHref) ? targetHref : `${SITE_ORIGIN}${targetHref}`;
  return [
    '<!doctype html>',
    '<html lang="en">',
    '<head>',
    '<meta charset="UTF-8" />',
    `<meta http-equiv="refresh" content="0; url=${targetHref}" />`,
    `<link rel="canonical" href="${canonicalHref}" />`,
    `<script>window.location.replace(${JSON.stringify(targetHref)})</script>`,
    '<title>Redirecting…</title>',
    '</head>',
    '<body></body>',
    '</html>',
  ].join('');
}

export function renderNotFoundPage(template: string): string {
  const rendered = renderRoute('/__ctx_missing__');
  return template
    .replace('<!--app-head-->', rendered.headHtml)
    .replace('<!--app-html-->', rendered.html);
}

export async function writeRedirect(
  pathname: string,
  target: string,
  outputRoot = distDir,
): Promise<void> {
  // Markdown aliases must remain Markdown on static preview hosts too. The
  // production host additionally serves the permanent redirect in _redirects.
  if (pathname.endsWith('.md')) {
    const outputPath = path.join(outputRoot, pathname.slice(1));
    await fs.mkdir(path.dirname(outputPath), { recursive: true });
    await fs.copyFile(path.join(outputRoot, target.slice(1)), outputPath);
    return;
  }
  const relativeDir = pathname === '/' ? '' : pathname.slice(1);
  const outputDir = path.join(outputRoot, relativeDir);
  const outputPath = path.join(outputDir, 'index.html');
  await fs.mkdir(outputDir, { recursive: true });
  await fs.writeFile(outputPath, renderRedirectPage(target), 'utf8');
}

async function writeNotFoundPage(template: string): Promise<void> {
  await fs.writeFile(path.join(distDir, '404.html'), renderNotFoundPage(template), 'utf8');
}

export function buildRedirectsFile(redirects: Record<string, string>): string {
  const redirectLines = Object.entries(redirects).map(
    ([pathname, target]) => `${pathname} ${toSiteHref(target)} 308`,
  );
  return `${redirectLines.join('\n')}\n`;
}

export function buildHeadersFile(): string {
  return [
    '/fonts/*',
    '  Access-Control-Allow-Origin: *',
    '  Cache-Control: public, max-age=31536000, immutable',
    '',
    '/assets/*',
    '  Cache-Control: public, max-age=31536000, immutable',
    '',
    '/search-index.json',
    '  Cache-Control: public, max-age=0, must-revalidate',
    '',
    '/index.md',
    '  X-Robots-Tag: noindex',
    '',
    '/*/*.md',
    '  X-Robots-Tag: noindex',
    '',
    '/robots.txt',
    '  Cache-Control: public, max-age=0, must-revalidate',
    '',
    '/sitemap.xml',
    '  Cache-Control: public, max-age=0, must-revalidate',
    '',
    '/llms.txt',
    '  Cache-Control: public, max-age=0, must-revalidate',
    '  X-Robots-Tag: noindex',
    '',
    '/.well-known/llms.txt',
    '  Cache-Control: public, max-age=0, must-revalidate',
    '  X-Robots-Tag: noindex',
    '',
    '/*',
    '  Link: </llms.txt>; rel="llms-txt"',
    '  Referrer-Policy: strict-origin-when-cross-origin',
    '  X-Llms-Txt: /llms.txt',
    '  X-Content-Type-Options: nosniff',
  ].join('\n');
}

async function writeCloudflareArtifacts(redirects: Record<string, string>): Promise<void> {
  await fs.writeFile(path.join(distDir, '_redirects'), buildRedirectsFile(redirects), 'utf8');
  await fs.writeFile(path.join(distDir, '_headers'), `${buildHeadersFile()}\n`, 'utf8');
}

async function main(): Promise<void> {
  const site = getSiteData();
  const template = await fs.readFile(templatePath, 'utf8');

  for (const pathname of site.pageOrder) {
    await writeRoute(template, pathname);
  }

  for (const [pathname, target] of Object.entries(site.redirects)) {
    await writeRedirect(pathname, target);
  }

  await writeNotFoundPage(template);
  await writeCloudflareArtifacts(site.redirects);
}

const isEntrypoint =
  process.argv[1] !== undefined && pathToFileURL(process.argv[1]).href === import.meta.url;

if (isEntrypoint) {
  void main();
}
