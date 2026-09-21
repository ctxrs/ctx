import { describe, expect, it } from 'vitest';
import {
  buildDiscoveryAssets,
  buildLlmsTxt,
  buildMarkdownExportContent,
  buildMarkdownExportPath,
  buildRobotsTxt,
  buildSitemapXml,
  type DiscoveryPage,
} from './discovery-assets';

const fixturePages: DiscoveryPage[] = [
  {
    description: 'Start here to install ctx and open your first workspace.',
    groupLabel: 'Getting started',
    lastModified: '2026-04-01T12:00:00.000Z',
    order: 0,
    pathname: '/',
    slug: 'index',
    tabLabel: 'Docs',
    title: 'Install and launch',
  },
  {
    description:
      'Understand local indexing, search, and citation flow.\nThis second sentence should be dropped.',
    groupLabel: 'Concepts',
    lastModified: '2026-04-01T13:00:00.000Z',
    order: 1,
    pathname: '/concepts/how-it-works',
    slug: 'concepts/how-it-works',
    tabLabel: 'Docs',
    title: 'How it works',
  },
  {
    description: 'Why agent workflows fail when merge and review do not stay coordinated.',
    groupLabel: 'Blog',
    lastModified: '2026-04-01T14:00:00.000Z',
    order: 2,
    pathname: '/blog/the-fermai-paradox',
    slug: 'blog/the-fermai-paradox',
    tabLabel: 'Blog',
    title: 'The Fermai Paradox',
  },
  {
    description: 'Legal terms for using the product.',
    groupLabel: 'Legal',
    lastModified: '2026-04-01T15:00:00.000Z',
    order: 3,
    pathname: '/legal/terms',
    slug: 'legal/terms',
    tabLabel: 'Docs',
    title: 'Terms of Use',
  },
];

describe('discovery assets', () => {
  it('builds Mintlify-style robots.txt output', () => {
    expect(buildRobotsTxt()).toBe(
      ['User-agent: *', 'Disallow: /cdn-cgi/', 'Sitemap: https://ctx.rs/sitemap.xml', ''].join('\n'),
    );
  });

  it('builds sitemap.xml from actual page routes', () => {
    const sitemap = buildSitemapXml(fixturePages);

    expect(sitemap).toContain('<loc>https://ctx.rs</loc>');
    expect(sitemap).toContain('<loc>https://ctx.rs/concepts/how-it-works/</loc>');
    expect(sitemap).toContain('<lastmod>2026-04-01T13:00:00.000Z</lastmod>');
    expect(sitemap).not.toContain('.md</loc>');
  });

  it('builds llms.txt with docs-first ordering and markdown export links', () => {
    const llms = buildLlmsTxt('ctx', fixturePages);

    expect(llms).toContain('# ctx');
    expect(llms).toContain('## Docs');
    expect(llms).toContain('## Blog');
    expect(llms).toContain('## Optional');
    expect(llms).toContain(`- [Install and launch](https://ctx.rs${buildMarkdownExportPath('index')}): Start here to install ctx and open your first workspace.`);
    expect(llms).toContain(`- [How it works](https://ctx.rs${buildMarkdownExportPath('concepts/how-it-works')}): Understand local indexing, search, and citation flow.`);
    expect(llms).not.toContain('This second sentence should be dropped.');
    expect(llms.indexOf('## Docs')).toBeLessThan(llms.indexOf('## Blog'));
    expect(llms.indexOf('## Blog')).toBeLessThan(llms.indexOf('## Optional'));
  });

  it('emits static asset paths for root and well-known discovery files', () => {
    expect(buildDiscoveryAssets('ctx', fixturePages).map((asset) => asset.relativePath)).toEqual([
      'robots.txt',
      'sitemap.xml',
      'llms.txt',
      '.well-known/llms.txt',
    ]);
  });

  it('builds markdown exports with a title when the source lacks an H1', () => {
    expect(buildMarkdownExportContent('Install and launch', 'Setup guide', '## Before you install')).toBe(
      ['# Install and launch', '', 'Setup guide', '', '## Before you install', ''].join('\n'),
    );
    expect(buildMarkdownExportContent('Install and launch', 'Setup guide', '# Existing title')).toBe(
      '# Existing title\n',
    );
  });
});
