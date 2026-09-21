import type { PageRecord, SiteData } from '../types';
import { toCanonicalPath } from './route-hrefs';
import { getPageForPathname, getResolvedPathname, getSiteData } from './site-data';
import { SITE_ORIGIN } from './site-origin';

const DYNAMIC_HEAD_ATTRIBUTE = 'data-ctx-route-head';
const DYNAMIC_HEAD_KEY_ATTRIBUTE = 'data-ctx-route-head-key';
const DEFAULT_SOCIAL_IMAGE_PATH = '/android-chrome-512x512.png';
const HOME_PAGE_SOCIAL_IMAGE_PATH = '/android-chrome-512x512.png';
const ORGANIZATION_NAME = 'ctx engineering inc';

const MONTHS = new Map<string, string>([
  ['january', '01'],
  ['february', '02'],
  ['march', '03'],
  ['april', '04'],
  ['may', '05'],
  ['june', '06'],
  ['july', '07'],
  ['august', '08'],
  ['september', '09'],
  ['october', '10'],
  ['november', '11'],
  ['december', '12'],
]);

type PageKind = 'blog' | 'docs' | 'home' | 'legal' | 'missing';
type HeadTagName = 'link' | 'meta' | 'script';

interface BreadcrumbItem {
  name: string;
  pathname?: string;
}

interface BlogArticleMetadata {
  authorName?: string;
  publishedAt?: string;
}

export interface HeadElementDescriptor {
  attributes: Record<string, string>;
  key: string;
  tagName: HeadTagName;
  textContent?: string;
}

export interface RouteMetadata {
  canonicalUrl: string;
  description: string;
  headElements: HeadElementDescriptor[];
  title: string;
}

function stripTags(value: string): string {
  return value.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ').trim();
}

function escapeHtmlText(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}

function escapeHtmlAttribute(value: string): string {
  return escapeHtmlText(value).replace(/"/g, '&quot;');
}

function escapeJsonForScript(value: string): string {
  return value.replace(/</g, '\\u003c');
}

function buildAssetUrl(pathname: string): string {
  if (/^https?:\/\//.test(pathname)) {
    return pathname;
  }

  return buildCanonicalUrl(pathname);
}

function getPageKind(page: PageRecord | null, pathname: string): PageKind {
  if (!page) {
    return 'missing';
  }

  if (pathname === '/') {
    return 'home';
  }

  if (page.tabKey === 'blog' || pathname.startsWith('/blog/')) {
    return 'blog';
  }

  if (pathname.startsWith('/legal/')) {
    return 'legal';
  }

  return 'docs';
}

function parseMonthDate(value: string): string | undefined {
  const match = value.trim().match(/^([A-Za-z]+)\s+(\d{1,2}),\s+(\d{4})$/);
  if (!match) {
    return undefined;
  }

  const [, monthName, dayValue, yearValue] = match;
  const month = MONTHS.get(monthName.toLowerCase());
  if (!month) {
    return undefined;
  }

  const day = Number(dayValue);
  if (!Number.isInteger(day) || day < 1 || day > 31) {
    return undefined;
  }

  return `${yearValue}-${month}-${dayValue.padStart(2, '0')}`;
}

function extractBlogArticleMetadata(page: PageRecord | null): BlogArticleMetadata {
  if (!page) {
    return {};
  }

  const match = page.html.match(/<p class="post-byline">\s*([^<]+?)\s+·\s+([^<]+?)\s*<\/p>/i);
  if (!match) {
    return {};
  }

  const authorName = stripTags(match[1]);
  const publishedAt = parseMonthDate(match[2]);

  return {
    authorName: authorName || undefined,
    publishedAt,
  };
}

function collectSameAs(site: SiteData): string[] {
  const values = Object.values(site.footer?.socials ?? {}).filter(Boolean);
  return [...new Set(values)];
}

function buildPublisher(site: SiteData): Record<string, unknown> {
  const publisher: Record<string, unknown> = {
    '@id': `${SITE_ORIGIN}/#organization`,
    '@type': 'Organization',
    legalName: ORGANIZATION_NAME,
    logo: {
      '@type': 'ImageObject',
      url: buildAssetUrl(site.theme.logoLight),
    },
    name: ORGANIZATION_NAME,
    url: SITE_ORIGIN,
  };

  const sameAs = collectSameAs(site);
  if (sameAs.length > 0) {
    publisher.sameAs = sameAs;
  }

  return publisher;
}

function buildOrganizationJsonLd(site: SiteData): Record<string, unknown> {
  const organization: Record<string, unknown> = {
    '@context': 'https://schema.org',
    '@id': `${SITE_ORIGIN}/#organization`,
    '@type': 'Organization',
    legalName: ORGANIZATION_NAME,
    logo: buildAssetUrl(site.theme.logoLight),
    name: ORGANIZATION_NAME,
    url: SITE_ORIGIN,
  };

  const sameAs = collectSameAs(site);
  if (sameAs.length > 0) {
    organization.sameAs = sameAs;
  }

  return organization;
}

function buildSoftwareApplicationJsonLd(description: string, site: SiteData): Record<string, unknown> {
  return {
    '@context': 'https://schema.org',
    '@type': 'SoftwareApplication',
    applicationCategory: 'DeveloperApplication',
    description,
    image: buildAssetUrl(HOME_PAGE_SOCIAL_IMAGE_PATH),
    isAccessibleForFree: true,
    name: site.siteName,
    operatingSystem: 'macOS, Linux',
    publisher: buildPublisher(site),
    url: SITE_ORIGIN,
  };
}

function findGroupPathname(page: PageRecord, site: SiteData): string | undefined {
  if (!page.groupLabel) {
    return undefined;
  }

  if (page.tabKey === 'blog') {
    return '/blog';
  }

  const tab = site.tabs.find((entry) => !entry.external && entry.key === page.tabKey);
  const group = tab?.groups.find((entry) => entry.label === page.groupLabel);
  const groupPathname =
    group?.pages[0] ??
    site.pageOrder.find((pathname) => {
      const candidate = site.pages[pathname];
      return candidate?.tabKey === page.tabKey && candidate.groupLabel === page.groupLabel;
    });

  if (!groupPathname || groupPathname === page.pathname) {
    return undefined;
  }

  return groupPathname;
}

function buildBreadcrumbItems(page: PageRecord | null, site: SiteData): BreadcrumbItem[] | null {
  if (!page || page.pathname === '/') {
    return null;
  }

  const items: BreadcrumbItem[] = [{ name: 'Home', pathname: '/' }];

  const groupPathname = findGroupPathname(page, site);
  if (page.groupLabel && page.groupLabel !== page.title && groupPathname) {
    items.push({
      name: page.groupLabel,
      pathname: groupPathname,
    });
  }

  items.push({
    name: page.title,
    pathname: page.pathname,
  });

  return items;
}

function buildBreadcrumbJsonLd(items: BreadcrumbItem[]): Record<string, unknown> {
  return {
    '@context': 'https://schema.org',
    '@type': 'BreadcrumbList',
    itemListElement: items.map((item, index) => ({
      '@type': 'ListItem',
      position: index + 1,
      name: item.name,
      ...(item.pathname ? { item: buildCanonicalUrl(item.pathname) } : {}),
    })),
  };
}

function buildBlogPostingJsonLd(
  canonicalUrl: string,
  description: string,
  page: PageRecord,
  site: SiteData,
): Record<string, unknown> {
  const articleMetadata = extractBlogArticleMetadata(page);
  const payload: Record<string, unknown> = {
    '@context': 'https://schema.org',
    '@type': 'BlogPosting',
    description,
    headline: page.title,
    image: buildAssetUrl(DEFAULT_SOCIAL_IMAGE_PATH),
    mainEntityOfPage: canonicalUrl,
    publisher: buildPublisher(site),
    url: canonicalUrl,
  };

  if (articleMetadata.authorName) {
    payload.author = {
      '@type': 'Person',
      name: articleMetadata.authorName,
    };
  }

  if (articleMetadata.publishedAt) {
    payload.dateModified = articleMetadata.publishedAt;
    payload.datePublished = articleMetadata.publishedAt;
  }

  return payload;
}

function buildStructuredData(
  canonicalUrl: string,
  description: string,
  page: PageRecord | null,
  site: SiteData,
): HeadElementDescriptor[] {
  const scripts: HeadElementDescriptor[] = [];
  const pathname = page?.pathname ?? '/';
  const pageKind = getPageKind(page, pathname);

  if (pageKind === 'home') {
    const graph = {
      '@context': 'https://schema.org',
      '@graph': [
        buildOrganizationJsonLd(site),
        buildSoftwareApplicationJsonLd(description, site),
      ],
    };

    scripts.push({
      attributes: { type: 'application/ld+json' },
      key: 'jsonld-home',
      tagName: 'script',
      textContent: JSON.stringify(graph),
    });
  }

  const breadcrumbItems = buildBreadcrumbItems(page, site);
  if (breadcrumbItems) {
    scripts.push({
      attributes: { type: 'application/ld+json' },
      key: 'jsonld-breadcrumbs',
      tagName: 'script',
      textContent: JSON.stringify(buildBreadcrumbJsonLd(breadcrumbItems)),
    });
  }

  if (pageKind === 'blog' && page) {
    scripts.push({
      attributes: { type: 'application/ld+json' },
      key: 'jsonld-blog-posting',
      tagName: 'script',
      textContent: JSON.stringify(buildBlogPostingJsonLd(canonicalUrl, description, page, site)),
    });
  }

  return scripts;
}

function createManagedHeadElement(descriptor: HeadElementDescriptor): HTMLElement {
  const element = document.createElement(descriptor.tagName);
  element.setAttribute(DYNAMIC_HEAD_ATTRIBUTE, 'true');
  element.setAttribute(DYNAMIC_HEAD_KEY_ATTRIBUTE, descriptor.key);

  for (const [name, value] of Object.entries(descriptor.attributes)) {
    element.setAttribute(name, value);
  }

  if (descriptor.tagName === 'script' && descriptor.textContent) {
    element.textContent = descriptor.textContent;
  }

  return element;
}

export function buildPageTitle(page: PageRecord | null): string {
  const site = getSiteData();
  if (!page) {
    return `Page not found - ${site.siteName}`;
  }
  if (page.pathname === '/') {
    return page.title;
  }
  return `${page.title} - ${site.siteName}`;
}

export function buildMetaDescription(page: PageRecord | null): string {
  const site = getSiteData();
  if (!page) {
    return 'This route is not part of the current docs set.';
  }

  const description = stripTags(page.descriptionHtml || page.description);
  return description || site.defaultDescription;
}

export function buildCanonicalUrl(pathname: string): string {
  const normalizedPathname = pathname === '/' ? '' : toCanonicalPath(pathname);
  return `${SITE_ORIGIN}${normalizedPathname}`;
}

export function buildRouteMetadata(pathname: string): RouteMetadata {
  const site = getSiteData();
  const resolvedPathname = getResolvedPathname(pathname);
  const page = getPageForPathname(resolvedPathname);
  const title = buildPageTitle(page);
  const description = buildMetaDescription(page);
  const canonicalUrl = buildCanonicalUrl(resolvedPathname);
  const pageKind = getPageKind(page, resolvedPathname);
  const isIndexable = pageKind !== 'missing';
  const articleMetadata = pageKind === 'blog' ? extractBlogArticleMetadata(page) : {};
  const socialImagePath =
    pageKind === 'home' ? HOME_PAGE_SOCIAL_IMAGE_PATH : DEFAULT_SOCIAL_IMAGE_PATH;
  const socialImageUrl = buildAssetUrl(socialImagePath);
  const socialImageAlt =
    pageKind === 'home'
      ? description
      : page
        ? `${page.title} on ${site.siteName}`
        : `${site.siteName} documentation preview`;

  const headElements: HeadElementDescriptor[] = [
    {
      attributes: { content: description, name: 'description' },
      key: 'meta-description',
      tagName: 'meta',
    },
  ];

  if (!isIndexable) {
    headElements.push({
      attributes: { content: 'noindex', name: 'robots' },
      key: 'meta-robots',
      tagName: 'meta',
    });
  } else {
    headElements.push(
      {
        attributes: { href: canonicalUrl, rel: 'canonical' },
        key: 'link-canonical',
        tagName: 'link',
      },
      {
        attributes: { content: pageKind === 'blog' ? 'article' : 'website', property: 'og:type' },
        key: 'meta-og-type',
        tagName: 'meta',
      },
      {
        attributes: { content: title, property: 'og:title' },
        key: 'meta-og-title',
        tagName: 'meta',
      },
      {
        attributes: { content: description, property: 'og:description' },
        key: 'meta-og-description',
        tagName: 'meta',
      },
      {
        attributes: { content: canonicalUrl, property: 'og:url' },
        key: 'meta-og-url',
        tagName: 'meta',
      },
      {
        attributes: { content: site.siteName, property: 'og:site_name' },
        key: 'meta-og-site-name',
        tagName: 'meta',
      },
      {
        attributes: { content: socialImageUrl, property: 'og:image' },
        key: 'meta-og-image',
        tagName: 'meta',
      },
      {
        attributes: { content: socialImageAlt, property: 'og:image:alt' },
        key: 'meta-og-image-alt',
        tagName: 'meta',
      },
      {
        attributes: { content: 'summary_large_image', name: 'twitter:card' },
        key: 'meta-twitter-card',
        tagName: 'meta',
      },
      {
        attributes: { content: title, name: 'twitter:title' },
        key: 'meta-twitter-title',
        tagName: 'meta',
      },
      {
        attributes: { content: description, name: 'twitter:description' },
        key: 'meta-twitter-description',
        tagName: 'meta',
      },
      {
        attributes: { content: socialImageUrl, name: 'twitter:image' },
        key: 'meta-twitter-image',
        tagName: 'meta',
      },
      {
        attributes: { content: socialImageAlt, name: 'twitter:image:alt' },
        key: 'meta-twitter-image-alt',
        tagName: 'meta',
      },
    );
  }

  if (articleMetadata.authorName) {
    headElements.push({
      attributes: { content: articleMetadata.authorName, property: 'article:author' },
      key: 'meta-article-author',
      tagName: 'meta',
    });
  }

  if (articleMetadata.publishedAt) {
    headElements.push({
      attributes: { content: articleMetadata.publishedAt, property: 'article:published_time' },
      key: 'meta-article-published-time',
      tagName: 'meta',
    });
  }

  headElements.push(...buildStructuredData(canonicalUrl, description, page, site));

  return {
    canonicalUrl,
    description,
    headElements,
    title,
  };
}

export function renderRouteHeadHtml(metadata: RouteMetadata): string {
  const tags = metadata.headElements.map((descriptor) => {
    const attributes = {
      ...descriptor.attributes,
      [DYNAMIC_HEAD_ATTRIBUTE]: 'true',
      [DYNAMIC_HEAD_KEY_ATTRIBUTE]: descriptor.key,
    };
    const serializedAttributes = Object.entries(attributes)
      .map(([name, value]) => ` ${name}="${escapeHtmlAttribute(value)}"`)
      .join('');

    if (descriptor.tagName === 'script') {
      return `<script${serializedAttributes}>${escapeJsonForScript(descriptor.textContent ?? '')}</script>`;
    }

    return `<${descriptor.tagName}${serializedAttributes} />`;
  });

  return [`<title>${escapeHtmlText(metadata.title)}</title>`, ...tags].join('');
}

export function applyRouteMetadata(pathname: string): void {
  if (typeof document === 'undefined') {
    return;
  }

  const metadata = buildRouteMetadata(pathname);
  document.title = metadata.title;

  for (const existingNode of document.head.querySelectorAll(`[${DYNAMIC_HEAD_ATTRIBUTE}="true"]`)) {
    existingNode.remove();
  }

  const fragment = document.createDocumentFragment();
  for (const descriptor of metadata.headElements) {
    fragment.appendChild(createManagedHeadElement(descriptor));
  }
  document.head.appendChild(fragment);
}
