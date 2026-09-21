import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { buildHeadersFile, buildRedirectsFile, renderNotFoundPage, renderRedirectPage, writeRedirect } from './prerender';

const template = [
  '<!doctype html>',
  '<html lang="en">',
  '<head>',
  '<!--app-head-->',
  '</head>',
  '<body>',
  '<div id="app"><!--app-html--></div>',
  '</body>',
  '</html>',
].join('');

describe('prerender helpers', () => {
  it('renders redirect pages with a canonical target and client redirect', () => {
    const html = renderRedirectPage('/reference/cli');

    expect(html).toContain('http-equiv="refresh"');
    expect(html).toContain('<link rel="canonical" href="https://ctx.rs/reference/cli/" />');
    expect(html).toContain('window.location.replace("/reference/cli/")');
  });

  it('renders external redirect pages with an external canonical target', () => {
    const html = renderRedirectPage('https://join.slack.com/t/ctxcommunity/shared_invite/example');

    expect(html).toContain(
      '<link rel="canonical" href="https://join.slack.com/t/ctxcommunity/shared_invite/example" />',
    );
    expect(html).toContain(
      'window.location.replace("https://join.slack.com/t/ctxcommunity/shared_invite/example")',
    );
  });

  it('retains permanent HTML and Markdown redirects without changing Markdown extensions', () => {
    expect(buildRedirectsFile({
      '/pro': '/blame',
      '/pro/index.md': '/blame.md',
      '/pro/referrals.md': '/index.md',
    })).toBe([
      '/pro /blame/ 308',
      '/pro/index.md /blame.md 308',
      '/pro/referrals.md /index.md 308',
      '',
    ].join('\n'));
  });

  it('serves the canonical Markdown bytes at old static export paths', async () => {
    const root = await fs.mkdtemp(path.join(os.tmpdir(), 'ctx-docs-redirect-'));
    try {
      const markdown = '# Blame\n\nBlame is included in ctx.\n';
      await fs.writeFile(path.join(root, 'blame.md'), markdown);
      await writeRedirect('/pro/index.md', '/blame.md', root);
      expect(await fs.readFile(path.join(root, 'pro/index.md'), 'utf8')).toBe(markdown);
    } finally {
      await fs.rm(root, { recursive: true, force: true });
    }
  });

  it('renders a dedicated not-found document with noindex metadata', () => {
    const html = renderNotFoundPage(template);

    expect(html).toContain('<title>Page not found - ctx</title>');
    expect(html).toContain('name="robots"');
    expect(html).toContain('content="noindex"');
    expect(html).toContain('<h1>Page not found</h1>');
    expect(html).not.toContain('rel="canonical"');
  });

  it('marks markdown exports and llms discovery files as noindex in host headers', () => {
    const headers = buildHeadersFile();

    expect(headers).toContain('/index.md');
    expect(headers).toContain('/*/*.md');
    expect(headers).toContain('/llms.txt');
    expect(headers).toContain('/.well-known/llms.txt');
    expect(headers).toContain('X-Robots-Tag: noindex');
  });

  it('serves shared fonts immutably with cross-origin access for other ctx services', () => {
    const headers = buildHeadersFile();

    expect(headers).toContain('/fonts/*');
    expect(headers).toContain('Access-Control-Allow-Origin: *');
    expect(headers).toContain('Cache-Control: public, max-age=31536000, immutable');
  });
});
