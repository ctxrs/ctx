import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { evaluate } from '@mdx-js/mdx';
import GithubSlugger from 'github-slugger';
import matter from 'gray-matter';
import { toString } from 'mdast-util-to-string';
import { Fragment, createElement, type ComponentType, type ReactNode } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import * as runtime from 'react/jsx-runtime';
import rehypeSlug from 'rehype-slug';
import remarkGfm from 'remark-gfm';
import remarkMdx from 'remark-mdx';
import remarkParse from 'remark-parse';
import { unified } from 'unified';
import { visit } from 'unist-util-visit';
import { transformCodeBlocksHtml } from '../src/lib/code-blocks';
import {
  buildDiscoveryAssets,
  buildMarkdownExportContent,
  buildMarkdownExportRelativePath,
  type DiscoveryPage,
  type GeneratedStaticAsset,
} from '../src/lib/discovery-assets';
import { toRouteHref } from '../src/lib/route-hrefs';
import { buildSearchIndex, type SearchPageSource } from '../src/lib/search-index-build';
import type { SearchIndexAsset } from '../src/lib/search-index';
import { SITE_ORIGIN } from '../src/lib/site-origin';
import type { FooterLinkGroup, FooterSocials, Heading, NavGroup, NavTab, PageRecord, SiteData } from '../src/types';

interface DocsConfigGroup {
  group: string;
  pages: string[];
}

interface DocsConfigExternalSource {
  environment: string;
  path: string;
}

interface DocsConfigTab {
  groups?: DocsConfigGroup[];
  href?: string;
  pages?: string[];
  tab: string;
}

interface DocsConfig {
  appearance?: { default?: 'dark' | 'light' };
  colors?: { dark?: string; light?: string; primary?: string };
  externalSources?: Record<string, DocsConfigExternalSource>;
  favicon?: string;
  footer?: { links?: FooterLinkGroup[]; socials?: FooterSocials };
  logo?: { dark?: string; light?: string };
  name?: string;
  navigation?: { tabs?: DocsConfigTab[] };
}

interface Frontmatter {
  description?: string;
  sidebarTitle?: string;
  title?: string;
}

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(scriptDir, '..');
const repoRoot = path.resolve(projectRoot, '../..');
const contentRoot = path.resolve(repoRoot, process.env.CTX_DOCS_CONTENT_ROOT ?? 'docs-content');
const generatedFile = path.resolve(projectRoot, 'src/generated/site-data.ts');
const publicDir = path.resolve(projectRoot, 'public');
const searchIndexFile = path.resolve(publicDir, 'search-index.json');

const assetExtensions = new Set(['.png', '.jpg', '.jpeg', '.svg', '.ico', '.webmanifest', '.ttf', '.woff2', '.mp4']);

interface MdxFrameProps {
  children?: ReactNode;
}

interface MdxCardGroupProps {
  children?: ReactNode;
  cols?: number;
}

interface MdxCardProps {
  children?: ReactNode;
  href?: string;
  title?: string;
}

function MdxFrame({ children }: MdxFrameProps) {
  return createElement('div', { className: 'ctx-doc-frame' }, children);
}

function MdxCardGroup({ children, cols }: MdxCardGroupProps) {
  const normalizedCols = cols === 2 ? 2 : 1;
  return createElement(
    'div',
    { className: `ctx-doc-card-group ctx-doc-card-group-cols-${normalizedCols}` },
    children,
  );
}

function MdxCard({ children, href, title }: MdxCardProps) {
  const content = [
    title ? createElement('div', { className: 'ctx-doc-card-title', key: 'title' }, title) : null,
    children ? createElement('div', { className: 'ctx-doc-card-body', key: 'body' }, children) : null,
  ];

  if (href) {
    return createElement('a', { className: 'ctx-doc-card', href }, content);
  }

  return createElement('div', { className: 'ctx-doc-card' }, content);
}

function toPathname(slug: string): string {
  if (slug.endsWith('/index')) {
    return `/${slug.slice(0, -'/index'.length)}`;
  }
  return slug === 'index' ? '/' : `/${slug}`;
}

function slugifyKey(value: string): string {
  return value.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/(^-|-$)/g, '');
}

function stripParagraphWrapper(html: string): string {
  if (html.startsWith('<p>') && html.endsWith('</p>')) {
    return html.slice(3, -4);
  }
  return html;
}

function normalizeHtmlText(value: string): string {
  return value
    .replace(/<[^>]*>/g, '')
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#x27;|&#39;/g, "'")
    .replace(/\s+/g, ' ')
    .trim();
}

function stripMatchingLeadingH1(html: string, title: string): string {
  const match = /^<h1(?:\s[^>]*)?>([\s\S]*?)<\/h1>\s*/.exec(html);
  if (!match) {
    return html;
  }

  return normalizeHtmlText(match[1]) === title ? html.slice(match[0].length) : html;
}

function normalizePageHtml(pathname: string, title: string, html: string): string {
  const normalizedHtml = stripMatchingLeadingH1(html, title);

  if (!pathname.startsWith('/blog/')) {
    return normalizedHtml;
  }

  return normalizedHtml
    .replace(
      /<p style="margin-top:-0\.9rem;margin-bottom:1rem"><p>(.*?)<\/p><\/p>/,
      '<p class="post-byline">$1</p>',
    )
    .replace(
      /<div style="height:1px;margin-top:1\.25rem;margin-bottom:2rem;background:rgba\(148, 163, 184, 0\.28\)"><\/div>/,
      '<div aria-hidden="true" class="post-divider"></div>',
    );
}

async function readDocsConfig(): Promise<DocsConfig> {
  const configPath = path.join(contentRoot, 'docs.json');
  const fileContents = await fs.readFile(configPath, 'utf8');
  return JSON.parse(fileContents) as DocsConfig;
}

function resolveExternalSource(
  slug: string,
  externalSources: Record<string, DocsConfigExternalSource>,
): string | null {
  const source = externalSources[slug];
  if (!source) {
    return null;
  }

  const sourceRootValue = process.env[source.environment];
  if (!sourceRootValue) {
    throw new Error(
      `Unable to resolve external content source for ${slug}: set ${source.environment}`,
    );
  }

  const sourceRoot = path.resolve(sourceRootValue);
  const sourcePath = path.resolve(sourceRoot, source.path);
  const relativePath = path.relative(sourceRoot, sourcePath);
  if (relativePath.startsWith('..') || path.isAbsolute(relativePath)) {
    throw new Error(`External content source for ${slug} escapes ${source.environment}`);
  }

  return sourcePath;
}

async function findPageSource(
  slug: string,
  externalSources: Record<string, DocsConfigExternalSource>,
): Promise<string> {
  const externalSource = resolveExternalSource(slug, externalSources);
  if (externalSource) {
    try {
      await fs.access(externalSource);
      return externalSource;
    } catch {
      throw new Error(`Unable to read external content source for ${slug}: ${externalSource}`);
    }
  }

  const candidates = [`${slug}.mdx`, `${slug}.md`].map((filePath) => path.join(contentRoot, filePath));

  for (const candidate of candidates) {
    try {
      await fs.access(candidate);
      return candidate;
    } catch {
      continue;
    }
  }

  throw new Error(`Unable to resolve content source for slug: ${slug}`);
}

async function renderMdx(source: string): Promise<string> {
  const evaluated = await evaluate(source, {
    ...runtime,
    Fragment,
    rehypePlugins: [rehypeSlug],
    remarkPlugins: [remarkGfm],
  });

  const Component = evaluated.default as ComponentType;
  const rendered = renderToStaticMarkup(
    createElement(Component, {
      components: {
        Card: MdxCard,
        CardGroup: MdxCardGroup,
        Frame: MdxFrame,
      },
    }),
  );
  return await transformCodeBlocksHtml(rendered);
}

function extractHeadings(source: string): Heading[] {
  const tree = unified().use(remarkParse).use(remarkMdx).use(remarkGfm).parse(source);
  const headings: Heading[] = [];
  const slugger = new GithubSlugger();

  visit(tree, 'heading', (node) => {
    if (node.depth !== 2 && node.depth !== 3) {
      return;
    }

    const text = toString(node).trim();
    if (text.length === 0) {
      return;
    }

    headings.push({
      depth: node.depth as 2 | 3,
      id: slugger.slug(text),
      text,
    });
  });

  return headings;
}

async function renderDescriptionHtml(description: string | undefined): Promise<string> {
  if (!description) {
    return '';
  }

  return stripParagraphWrapper(await renderMdx(description));
}

function collectRoutes(tabs: DocsConfigTab[]): {
  pageSpecs: Array<{ groupLabel: string | null; slug: string; tabKey: string; tabLabel: string }>;
  redirects: Record<string, string>;
  pageTabs: NavTab[];
} {
  const pageTabs: NavTab[] = [];
  const pageSpecs: Array<{ groupLabel: string | null; slug: string; tabKey: string; tabLabel: string }> = [];
  const redirects: Record<string, string> = {};

  for (const tab of tabs) {
    const tabKey = slugifyKey(tab.tab);
    const groups: NavGroup[] = [];

    if (tab.href) {
      pageTabs.push({
        external: true,
        groups: [],
        href: tab.href,
        key: tabKey,
        label: tab.tab,
      });
      continue;
    }

    if (tab.groups) {
      for (const group of tab.groups) {
        const groupPathnames = group.pages.map(toPathname);
        groups.push({ label: group.group, pages: groupPathnames });

        for (const slug of group.pages) {
          pageSpecs.push({
            groupLabel: group.group,
            slug,
            tabKey,
            tabLabel: tab.tab,
          });
        }
      }
    }

    if (tab.pages) {
      const pathnames = tab.pages.map(toPathname);
      groups.push({ label: tab.tab, pages: pathnames });

      for (const slug of tab.pages) {
        pageSpecs.push({
          groupLabel: tab.tab,
          slug,
          tabKey,
          tabLabel: tab.tab,
        });
      }

      if (tab.tab.toLowerCase() === 'blog' && tab.pages[0]) {
        redirects['/blog'] = toPathname(tab.pages[0]);
      }
    }

    const firstRoute = groups[0]?.pages[0] ?? '/';
    pageTabs.push({
      external: false,
      groups,
      href: toRouteHref(firstRoute),
      key: tabKey,
      label: tab.tab,
    });
  }

  return { pageSpecs, redirects, pageTabs };
}

async function appendImplicitPageSpecs(
  pageSpecs: Array<{ groupLabel: string | null; slug: string; tabKey: string; tabLabel: string }>,
): Promise<Array<{ groupLabel: string | null; slug: string; tabKey: string; tabLabel: string }>> {
  const seen = new Set(pageSpecs.map((pageSpec) => pageSpec.slug));
  const legalDir = path.join(contentRoot, 'legal');

  try {
    const entries = await fs.readdir(legalDir, { withFileTypes: true });
    for (const entry of entries) {
      if (!entry.isFile()) {
        continue;
      }

      const extension = path.extname(entry.name).toLowerCase();
      if (extension !== '.md' && extension !== '.mdx') {
        continue;
      }

      const slug = `legal/${path.basename(entry.name, extension)}`;
      if (seen.has(slug)) {
        continue;
      }

      pageSpecs.push({
        groupLabel: 'Legal',
        slug,
        tabKey: 'docs',
        tabLabel: 'Docs',
      });
      seen.add(slug);
    }
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT') {
      throw error;
    }
  }

  return pageSpecs;
}

async function buildPages(
  pageSpecs: Array<{ groupLabel: string | null; slug: string; tabKey: string; tabLabel: string }>,
  externalSources: Record<string, DocsConfigExternalSource>,
): Promise<{
  discoveryPages: DiscoveryPage[];
  markdownAssets: GeneratedStaticAsset[];
  pageOrder: string[];
  pages: Record<string, PageRecord>;
  searchPages: SearchPageSource[];
}> {
  const discoveryPages: DiscoveryPage[] = [];
  const markdownAssets: GeneratedStaticAsset[] = [];
  const pageOrder: string[] = [];
  const pages: Record<string, PageRecord> = {};
  const searchPages: SearchPageSource[] = [];

  for (const [index, pageSpec] of pageSpecs.entries()) {
    const sourcePath = await findPageSource(pageSpec.slug, externalSources);
    const sourceFile = await fs.readFile(sourcePath, 'utf8');
    const sourceStats = await fs.stat(sourcePath);
    const parsed = matter(sourceFile);
    const frontmatter = parsed.data as Frontmatter;
    const pathname = toPathname(pageSpec.slug);
    const description = frontmatter.description ?? '';
    const title = frontmatter.title ?? pageSpec.slug;

    pages[pathname] = {
      description,
      descriptionHtml: await renderDescriptionHtml(description),
      groupLabel: pageSpec.groupLabel,
      headings: extractHeadings(parsed.content),
      html: normalizePageHtml(pathname, title, await renderMdx(parsed.content)),
      href: toRouteHref(pathname),
      index,
      pathname,
      sidebarTitle: frontmatter.sidebarTitle ?? frontmatter.title ?? pageSpec.slug,
      slug: pageSpec.slug,
      sourcePath: path.relative(repoRoot, sourcePath),
      tabKey: pageSpec.tabKey,
      tabLabel: pageSpec.tabLabel,
      title,
    };

    searchPages.push({
      description,
      groupLabel: pageSpec.groupLabel,
      order: index,
      pathname,
      source: parsed.content,
      tabLabel: pageSpec.tabLabel,
      title,
    });

    discoveryPages.push({
      description,
      groupLabel: pageSpec.groupLabel,
      lastModified: sourceStats.mtime.toISOString(),
      order: index,
      pathname,
      slug: pageSpec.slug,
      tabLabel: pageSpec.tabLabel,
      title,
    });

    markdownAssets.push({
      content: buildMarkdownExportContent(title, description, parsed.content),
      relativePath: buildMarkdownExportRelativePath(pageSpec.slug),
    });

    pageOrder.push(pathname);
  }

  return { discoveryPages, markdownAssets, pageOrder, pages, searchPages };
}

async function ensureCleanPublicDir(): Promise<void> {
  await fs.mkdir(publicDir, { recursive: true });
  // Renderer-owned fonts and logos must survive builds from source archives and
  // Bazel runfiles, where Git metadata is absent. Content assets are copied below.
  const preservedEntries = new Set(['.gitkeep', 'fonts', 'logo']);
  const existingEntries = await fs.readdir(publicDir, { withFileTypes: true });

  for (const entry of existingEntries) {
    if (preservedEntries.has(entry.name)) {
      continue;
    }

    await fs.rm(path.join(publicDir, entry.name), { force: true, recursive: true });
  }
}

async function copyAssets(sourceDir: string, targetDir: string): Promise<void> {
  const entries = await fs.readdir(sourceDir, { withFileTypes: true });

  for (const entry of entries) {
    const sourcePath = path.join(sourceDir, entry.name);
    const targetPath = path.join(targetDir, entry.name);

    if (entry.isDirectory()) {
      await fs.mkdir(targetPath, { recursive: true });
      await copyAssets(sourcePath, targetPath);
      continue;
    }

    if (!assetExtensions.has(path.extname(entry.name).toLowerCase())) {
      continue;
    }

    await fs.mkdir(path.dirname(targetPath), { recursive: true });
    await fs.copyFile(sourcePath, targetPath);
  }
}

async function writeGeneratedModule(siteData: SiteData): Promise<void> {
  const fileContents = `import type { SiteData } from '../types';\n\nexport const siteData: SiteData = ${JSON.stringify(siteData, null, 2)};\n`;
  await fs.mkdir(path.dirname(generatedFile), { recursive: true });
  await fs.writeFile(generatedFile, fileContents, 'utf8');
}

async function writeSearchIndexAsset(searchIndex: SearchIndexAsset): Promise<void> {
  await fs.writeFile(searchIndexFile, JSON.stringify(searchIndex), 'utf8');
}

async function writeGeneratedStaticAssets(assets: GeneratedStaticAsset[]): Promise<void> {
  for (const asset of assets) {
    const outputPath = path.join(publicDir, asset.relativePath);
    await fs.mkdir(path.dirname(outputPath), { recursive: true });
    await fs.writeFile(outputPath, asset.content, 'utf8');
  }
}

async function main(): Promise<void> {
  const docsConfig = await readDocsConfig();
  const tabs = docsConfig.navigation?.tabs ?? [];
  const { pageSpecs: basePageSpecs, redirects, pageTabs } = collectRoutes(tabs);
  redirects['/cloud'] = '/teams';
  redirects['/cloud/'] = '/teams';
  if (docsConfig.footer?.socials?.slack) {
    redirects['/slack'] = docsConfig.footer.socials.slack;
  }
  const pageSpecs = await appendImplicitPageSpecs(basePageSpecs);
  const { discoveryPages, markdownAssets, pageOrder, pages, searchPages } = await buildPages(
    pageSpecs,
    docsConfig.externalSources ?? {},
  );
  // Product aliases apply only when this content root contains the current guide.
  // An explicitly supplied archive root keeps its own routes.
  if (pages['/blame']) {
    for (const pathname of ['/pro', '/pro/', '/pro/index', '/pro/index/']) {
      redirects[pathname] = '/blame';
    }
    redirects['/pro/index.md'] = '/blame.md';
    redirects['/pro.md'] = '/blame.md';
    for (const pathname of ['/pro/referrals', '/pro/referrals/']) {
      redirects[pathname] = '/legal/legacy-services';
    }
    redirects['/pro/referrals.md'] = '/legal/legacy-services.md';
  }
  const searchIndex = buildSearchIndex(searchPages);
  const siteName = docsConfig.name ?? 'ctx Docs';

  const siteData: SiteData = {
    defaultDescription:
      'ctx indexes local agent-history so future agents can retrieve prior work with citations.',
    footer: docsConfig.footer
      ? {
          links: docsConfig.footer.links ?? [],
          socials: docsConfig.footer.socials ?? {},
        }
      : null,
    pageOrder,
    pages,
    redirects,
    siteName,
    tabs: pageTabs,
    theme: {
      dark: docsConfig.colors?.dark ?? '#166534',
      defaultAppearance: docsConfig.appearance?.default ?? 'dark',
      favicon: docsConfig.favicon ?? '/favicon.ico',
      light: docsConfig.colors?.light ?? '#33FF33',
      logoDark: docsConfig.logo?.dark ?? '/favicon.ico',
      logoLight: docsConfig.logo?.light ?? '/favicon.ico',
      primary: docsConfig.colors?.primary ?? '#15803D',
    },
  };

  await writeGeneratedModule(siteData);
  await ensureCleanPublicDir();
  await copyAssets(contentRoot, publicDir);
  await writeSearchIndexAsset(searchIndex);
  await writeGeneratedStaticAssets([
    ...markdownAssets,
    ...buildDiscoveryAssets(siteName, discoveryPages, SITE_ORIGIN),
  ]);
}

void main();
