import type { SearchIndexEntry } from './search-index';
import { tokenizeSearchQuery } from './search-text';

export interface SearchHighlightSegment {
  matched: boolean;
  text: string;
}

export function buildSearchResultBreadcrumbs(entry: SearchIndexEntry): string[] {
  if (entry.kind === 'section' && entry.sectionTitle) {
    return [entry.pageTitle, entry.sectionTitle].filter((part) => part.trim().length > 0);
  }

  const leadingLabel = entry.groupLabel ?? entry.tabLabel;
  return [leadingLabel, entry.pageTitle].filter((part) => part.trim().length > 0);
}

export function buildSearchResultSnippet(entry: SearchIndexEntry): string {
  const title = entry.title.trim();
  const excerpt = entry.excerpt.trim();
  if (excerpt.length === 0) {
    return title;
  }

  if (excerpt.toLowerCase().startsWith(title.toLowerCase())) {
    return excerpt;
  }

  return `${title} ${excerpt}`;
}

export function buildSearchHighlightSegments(text: string, query: string): SearchHighlightSegment[] {
  const highlightTerms = tokenizeSearchQuery(query);
  if (highlightTerms.length === 0 || text.length === 0) {
    return text.length === 0 ? [] : [{ matched: false, text }];
  }

  const pattern = new RegExp(
    `(${highlightTerms
      .slice()
      .sort((left, right) => right.length - left.length)
      .map((token) => token.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'))
      .join('|')})`,
    'ig',
  );

  return text
    .split(pattern)
    .filter((part) => part.length > 0)
    .map((part) => ({
      matched: highlightTerms.includes(part.toLowerCase()),
      text: part,
    }));
}
