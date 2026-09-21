import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

const srcDir = path.dirname(fileURLToPath(import.meta.url));
const indexCss = fs.readFileSync(path.resolve(srcDir, 'index.css'), 'utf8');
const indexHtml = fs.readFileSync(path.resolve(srcDir, '../index.html'), 'utf8');
const brandFontsSource = fs.readFileSync(
  path.resolve(srcDir, 'lib/brand-fonts.ts'),
  'utf8',
);
const searchDialogCss = fs.readFileSync(
  path.resolve(srcDir, 'components/SearchDialog.css'),
  'utf8',
);
const searchDialogSource = fs.readFileSync(
  path.resolve(srcDir, 'components/SearchDialog.tsx'),
  'utf8',
);
const vt323Woff2Path = path.resolve(srcDir, '../public/fonts/vt323-regular.woff2');

describe('performance-oriented asset loading', () => {
  it('keeps search dialog styles in a component-scoped lazy stylesheet', () => {
    expect(searchDialogSource).toContain("import './SearchDialog.css';");
    expect(searchDialogCss).toContain('.search-dialog-backdrop {');
    expect(searchDialogCss).toContain('@media (max-width: 920px)');
    expect(indexCss).not.toContain('.search-dialog-backdrop {');
    expect(indexCss).not.toContain('.search-results-panel {');
  });

  it('uses a non-blocking font-display setting for the code font', () => {
    expect(indexCss).toMatch(/font-family: "JetBrains Mono";[\s\S]*?font-display: optional;/);
  });

  it('uses the gated WOFF2 brand families', () => {
    expect(indexCss).toContain('font-family: "ctx Inter", "Inter Fallback"');
    expect(indexCss).toContain('font-family: "ctx VT323", "Courier New"');
    expect(fs.statSync(vt323Woff2Path).size).toBeLessThan(40_000);
  });

  it('preloads and gates the first paint until the brand fonts settle', () => {
    expect(indexHtml).toContain('href="/fonts/inter-variable-latin.woff2"');
    expect(indexHtml).toContain('href="/fonts/inter-variable-italic-latin.woff2"');
    expect(indexHtml).toContain('href="/fonts/vt323-regular.woff2"');
    expect(indexHtml).toContain('html.ctx-fonts-loading #app');
    expect(brandFontsSource).toContain('const BRAND_FONT_TIMEOUT_MS = 1_500;');
    expect(brandFontsSource).toContain("new FontFace(\n      'ctx Inter'");
    expect(brandFontsSource).toContain("new FontFace(\n      'ctx VT323'");
  });
});
