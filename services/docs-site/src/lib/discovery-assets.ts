import { SITE_ORIGIN } from './site-origin';
import { toCanonicalPath } from './route-hrefs';

export interface DiscoveryPage {
  description: string;
  groupLabel: string | null;
  lastModified?: string;
  order: number;
  pathname: string;
  slug: string;
  tabLabel: string;
  title: string;
}

export interface GeneratedStaticAsset {
  content: string;
  relativePath: string;
}

const LLMS_SECTION_ORDER = ['Docs', 'Blog', 'Optional'] as const;

function trimTrailingSlash(value: string): string {
  return value.replace(/\/+$/, '');
}

function buildAbsoluteUrl(pathname: string, origin = SITE_ORIGIN): string {
  const normalizedOrigin = trimTrailingSlash(origin);
  return pathname === '/' ? normalizedOrigin : `${normalizedOrigin}${toCanonicalPath(pathname)}`;
}

function escapeXml(value: string): string {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&apos;');
}

function normalizeLlmsDescription(value: string): string {
  const firstLine = value.split(/\r?\n/, 1)[0]?.trim().replace(/\s+/g, ' ') ?? '';
  if (firstLine.length <= 300) {
    return firstLine;
  }

  return `${firstLine.slice(0, 297).trimEnd()}...`;
}

function getLlmsSectionLabel(page: DiscoveryPage): (typeof LLMS_SECTION_ORDER)[number] {
  if (page.pathname.startsWith('/legal/')) {
    return 'Optional';
  }

  if (page.tabLabel === 'Blog') {
    return 'Blog';
  }

  return 'Docs';
}

export function buildMarkdownExportPath(slug: string): string {
  return slug === 'index' ? '/index.md' : `/${slug}.md`;
}

export function buildMarkdownExportRelativePath(slug: string): string {
  return buildMarkdownExportPath(slug).slice(1);
}

export function buildMarkdownExportContent(title: string, description: string, content: string): string {
  const trimmedContent = content.trim();
  if (/^#\s+/m.test(trimmedContent)) {
    return `${trimmedContent}\n`;
  }

  const trimmedDescription = description.trim();
  const parts = [`# ${title}`];

  if (trimmedDescription) {
    parts.push('', trimmedDescription);
  }

  if (trimmedContent) {
    parts.push('', trimmedContent);
  }

  return `${parts.join('\n')}\n`;
}

export function buildRobotsTxt(origin = SITE_ORIGIN): string {
  return ['User-agent: *', 'Disallow: /cdn-cgi/', `Sitemap: ${buildAbsoluteUrl('/sitemap.xml', origin)}`, ''].join('\n');
}

export function buildSitemapXml(pages: DiscoveryPage[], origin = SITE_ORIGIN): string {
  const sortedPages = [...pages].sort((left, right) => left.order - right.order);
  const urlEntries = sortedPages
    .map((page) => {
      const lines = ['  <url>', `    <loc>${escapeXml(buildAbsoluteUrl(page.pathname, origin))}</loc>`];

      if (page.lastModified) {
        lines.push(`    <lastmod>${escapeXml(page.lastModified)}</lastmod>`);
      }

      lines.push('  </url>');
      return lines.join('\n');
    })
    .join('\n\n');

  return [
    '<?xml version="1.0" encoding="UTF-8"?>',
    '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:news="http://www.google.com/schemas/sitemap-news/0.9" xmlns:xhtml="http://www.w3.org/1999/xhtml" xmlns:image="http://www.google.com/schemas/sitemap-image/1.1" xmlns:video="http://www.google.com/schemas/sitemap-video/1.1">',
    urlEntries,
    '</urlset>',
    '',
  ].join('\n');
}

export function buildLlmsTxt(siteName: string, pages: DiscoveryPage[], origin = SITE_ORIGIN): string {
  const sectionPages = new Map<(typeof LLMS_SECTION_ORDER)[number], DiscoveryPage[]>();

  for (const section of LLMS_SECTION_ORDER) {
    sectionPages.set(section, []);
  }

  for (const page of [...pages].sort((left, right) => left.order - right.order)) {
    sectionPages.get(getLlmsSectionLabel(page))?.push(page);
  }

  const lines: string[] = [`# ${siteName}`];

  for (const section of LLMS_SECTION_ORDER) {
    const entries = sectionPages.get(section);
    if (!entries || entries.length === 0) {
      continue;
    }

    lines.push('', `## ${section}`, '');

    for (const page of entries) {
      const href = buildAbsoluteUrl(buildMarkdownExportPath(page.slug), origin);
      const description = normalizeLlmsDescription(page.description);
      const line = description
        ? `- [${page.title}](${href}): ${description}`
        : `- [${page.title}](${href})`;
      lines.push(line);
    }
  }

  lines.push('');
  return lines.join('\n');
}

export function buildDiscoveryAssets(
  siteName: string,
  pages: DiscoveryPage[],
  origin = SITE_ORIGIN,
): GeneratedStaticAsset[] {
  const llmsTxt = buildLlmsTxt(siteName, pages, origin);

  return [
    { content: buildRobotsTxt(origin), relativePath: 'robots.txt' },
    { content: buildSitemapXml(pages, origin), relativePath: 'sitemap.xml' },
    { content: llmsTxt, relativePath: 'llms.txt' },
    { content: llmsTxt, relativePath: '.well-known/llms.txt' },
  ];
}
