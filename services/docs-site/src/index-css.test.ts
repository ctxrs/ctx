import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

const cssPath = path.resolve(path.dirname(fileURLToPath(import.meta.url)), 'index.css');
const css = fs.readFileSync(cssPath, 'utf8');

describe('index.css doc cards', () => {
  it('removes global link underlines from linked doc cards', () => {
    expect(css).toContain('.page-content a.ctx-doc-card');
    expect(css).toContain('text-decoration: none;');
  });
});

describe('index.css header install CTA', () => {
  it('uses a pointer cursor across the full copy control', () => {
    expect(css).toContain('.header-install-copy {');
    expect(css).toContain('cursor: pointer;');
    expect(css).toContain('.header-install-copy * {');
    expect(css).toContain('cursor: inherit;');
  });
});
