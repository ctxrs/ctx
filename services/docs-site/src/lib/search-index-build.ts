import GithubSlugger from 'github-slugger';
import { toString } from 'mdast-util-to-string';
import type { Heading, ListItem, Root, RootContent } from 'mdast';
import remarkGfm from 'remark-gfm';
import remarkMdx from 'remark-mdx';
import remarkParse from 'remark-parse';
import { unified } from 'unified';
import {
  SEARCH_INDEX_VERSION,
  type SearchIndexAsset,
  type SearchIndexEntry,
} from './search-index';
import { toRouteHref } from './route-hrefs';

export interface SearchPageSource {
  description: string;
  groupLabel: string | null;
  order: number;
  pathname: string;
  source: string;
  tabLabel: string;
  title: string;
}

export interface SearchSection {
  body: string;
  depth: 2 | 3;
  id: string;
  title: string;
}

interface SectionBuffer {
  bodyParts: string[];
  depth: 2 | 3;
  id: string;
  title: string;
}

function collapseWhitespace(value: string): string {
  return value.replace(/\s+/g, ' ').trim();
}

function toExcerpt(value: string, maxLength: number): string {
  const collapsed = collapseWhitespace(value);
  if (collapsed.length <= maxLength) {
    return collapsed;
  }

  const sliced = collapsed.slice(0, maxLength);
  const trailingWordBoundary = sliced.lastIndexOf(' ');
  if (trailingWordBoundary > maxLength * 0.6) {
    return `${sliced.slice(0, trailingWordBoundary).trim()}…`;
  }

  return `${sliced.trim()}…`;
}

function toPathKeywords(pathname: string): string[] {
  return pathname
    .split('/')
    .map((part) => part.replace(/[-_]+/g, ' ').trim())
    .filter((part) => part.length > 0);
}

function buildMarkdownTree(source: string): Root {
  return unified().use(remarkParse).use(remarkMdx).use(remarkGfm).parse(source) as Root;
}

function extractNodeText(node: RootContent | ListItem): string {
  if (node.type === 'list') {
    return collapseWhitespace(node.children.map((child) => extractNodeText(child)).join(' '));
  }

  if (node.type === 'listItem') {
    return collapseWhitespace(node.children.map((child) => extractNodeText(child)).join(' '));
  }

  return collapseWhitespace(toString(node));
}

export function extractSearchSections(source: string): { preamble: string; sections: SearchSection[] } {
  const tree = buildMarkdownTree(source);
  const slugger = new GithubSlugger();
  const preambleParts: string[] = [];
  const sectionBuffers: SectionBuffer[] = [];
  let currentSection: SectionBuffer | null = null;

  for (const child of tree.children) {
    if (child.type === 'heading' && (child.depth === 2 || child.depth === 3)) {
      const heading = child as Heading;
      const headingText = collapseWhitespace(toString(heading));
      if (headingText.length === 0) {
        currentSection = null;
        continue;
      }

      currentSection = {
        bodyParts: [],
        depth: heading.depth,
        id: slugger.slug(headingText),
        title: headingText,
      };
      sectionBuffers.push(currentSection);
      continue;
    }

    const text = extractNodeText(child);
    if (text.length === 0) {
      continue;
    }

    if (currentSection) {
      currentSection.bodyParts.push(text);
      continue;
    }

    preambleParts.push(text);
  }

  return {
    preamble: collapseWhitespace(preambleParts.join(' ')),
    sections: sectionBuffers.map((section) => ({
      body: collapseWhitespace(section.bodyParts.join(' ')),
      depth: section.depth,
      id: section.id,
      title: section.title,
    })),
  };
}

function trimBody(value: string, maxLength: number): string {
  const collapsed = collapseWhitespace(value);
  if (collapsed.length <= maxLength) {
    return collapsed;
  }

  return collapsed.slice(0, maxLength).trim();
}

export function buildSearchIndex(pages: SearchPageSource[]): SearchIndexAsset {
  const items: SearchIndexEntry[] = [];

  for (const page of pages) {
    const { preamble, sections } = extractSearchSections(page.source);
    const pageBodySource = [page.description, preamble, ...sections.map((section) => `${section.title} ${section.body}`)]
      .filter((part) => collapseWhitespace(part).length > 0)
      .join(' ');
    const pageBody = trimBody(pageBodySource, 1600);
    const pageExcerptSource = page.description || preamble || sections[0]?.body || page.title;
    const baseKeywords = [
      page.title,
      page.groupLabel ?? '',
      page.tabLabel,
      ...toPathKeywords(page.pathname),
    ].filter((keyword) => collapseWhitespace(keyword).length > 0);

    items.push({
      body: pageBody,
      excerpt: toExcerpt(pageExcerptSource, 180),
      groupLabel: page.groupLabel,
      href: toRouteHref(page.pathname),
      kind: 'page',
      keywords: baseKeywords,
      order: page.order,
      pageTitle: page.title,
      pathname: page.pathname,
      sectionTitle: null,
      tabLabel: page.tabLabel,
      title: page.title,
    });

    for (const [sectionIndex, section] of sections.entries()) {
      const sectionBody = trimBody(section.body || pageBody, 1200);
      const sectionExcerptSource = section.body || page.description || preamble || page.title;

      items.push({
        body: sectionBody,
        excerpt: toExcerpt(sectionExcerptSource, 180),
        groupLabel: page.groupLabel,
        href: `${toRouteHref(page.pathname)}#${section.id}`,
        kind: 'section',
        keywords: [...baseKeywords, section.title],
        order: page.order + (sectionIndex + 1) / 100,
        pageTitle: page.title,
        pathname: page.pathname,
        sectionTitle: section.title,
        tabLabel: page.tabLabel,
        title: section.title,
      });
    }
  }

  return {
    items,
    version: SEARCH_INDEX_VERSION,
  };
}
