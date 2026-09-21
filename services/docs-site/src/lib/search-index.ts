import {
  expandSearchTokenVariants,
  getSearchTextMatchTier,
  normalizeSearchText,
  tokenizeSearchQuery,
} from './search-text';

export const SEARCH_INDEX_ASSET_PATH = '/search-index.json';
export const SEARCH_INDEX_VERSION = 1;

export interface SearchIndexEntry {
  body: string;
  excerpt: string;
  groupLabel: string | null;
  href: string;
  kind: 'page' | 'section';
  keywords: string[];
  order: number;
  pageTitle: string;
  pathname: string;
  sectionTitle: string | null;
  tabLabel: string;
  title: string;
}

export interface SearchIndexAsset {
  items: SearchIndexEntry[];
  version: typeof SEARCH_INDEX_VERSION;
}

export interface SearchResult {
  entry: SearchIndexEntry;
  score: number;
}

const DEFAULT_RESULT_LIMIT = 6;
const GENERIC_SECTION_TITLE_PENALTIES = new Map<string, number>([
  ['overview', 48],
  ['related pages', 96],
]);

function scoreField(haystack: string, token: string): number {
  if (haystack.length === 0) {
    return 0;
  }

  if (haystack === token) {
    return 18;
  }

  if (haystack.startsWith(token)) {
    return 14;
  }

  const boundaryMatch = haystack.match(new RegExp(`(^|[ /_-])${token}`));
  if (boundaryMatch) {
    return 10;
  }

  if (haystack.includes(token)) {
    return 6;
  }

  return 0;
}

function buildDefaultResults(index: SearchIndexAsset, limit: number): SearchResult[] {
  void index;
  void limit;
  return [];
}

function getGenericSectionPenalty(entry: SearchIndexEntry, query: string): number {
  if (entry.kind !== 'section' || !entry.sectionTitle) {
    return 0;
  }

  if (getSearchTextMatchTier(entry.sectionTitle, query) > 0) {
    return 0;
  }

  return GENERIC_SECTION_TITLE_PENALTIES.get(normalizeSearchText(entry.sectionTitle)) ?? 0;
}

function scoreEntry(entry: SearchIndexEntry, query: string, tokens: string[]): number {
  const normalizedTitle = normalizeSearchText(entry.title);
  const normalizedPageTitle = normalizeSearchText(entry.pageTitle);
  const normalizedSectionTitle = normalizeSearchText(entry.sectionTitle ?? '');
  const normalizedGroup = normalizeSearchText(entry.groupLabel ?? '');
  const normalizedExcerpt = normalizeSearchText(entry.excerpt);
  const normalizedBody = normalizeSearchText(entry.body);
  const normalizedPath = normalizeSearchText(entry.pathname);
  const normalizedKeywords = normalizeSearchText(entry.keywords.join(' '));
  const normalizedQuery = normalizeSearchText(query);
  const ownTitle =
    entry.kind === 'section' && normalizedSectionTitle.length > 0
      ? normalizedSectionTitle
      : normalizedPageTitle;

  let score = 0;
  let matchedTokens = 0;

  for (const token of tokens) {
    let tokenScore = 0;

    for (const variant of expandSearchTokenVariants(token)) {
      const variantWeight = variant === token ? 1 : 0.42;
      const titleScore = scoreField(normalizedTitle, variant) * 8;
      const sectionScore = scoreField(normalizedSectionTitle, variant) * 7;
      const pageScore = scoreField(normalizedPageTitle, variant) * 6;
      const keywordScore = scoreField(normalizedKeywords, variant) * 5;
      const groupScore = scoreField(normalizedGroup, variant) * 4;
      const excerptScore = scoreField(normalizedExcerpt, variant) * 3;
      const pathScore = scoreField(normalizedPath, variant) * 3;
      const bodyScore = scoreField(normalizedBody, variant);
      const exactVariantBonus =
        variant === token
          ? 0
          : (normalizedTitle === variant ? 92 : 0) + (normalizedSectionTitle === variant ? 84 : 0);

      tokenScore = Math.max(
        tokenScore,
        (
          titleScore +
          sectionScore +
          pageScore +
          keywordScore +
          groupScore +
          excerptScore +
          pathScore +
          bodyScore
        ) *
          variantWeight +
          exactVariantBonus,
      );
    }

    if (tokenScore > 0) {
      matchedTokens += 1;
      score += tokenScore;
      continue;
    }

    score -= 24;
  }

  if (matchedTokens === 0) {
    return Number.NEGATIVE_INFINITY;
  }

  if (normalizedQuery.length > 0) {
    if (normalizedTitle === normalizedQuery) {
      score += 180;
    } else if (normalizedTitle.startsWith(normalizedQuery)) {
      score += 130;
    } else if (normalizedTitle.includes(normalizedQuery)) {
      score += 82;
    }

    if (normalizedSectionTitle === normalizedQuery) {
      score += 160;
    } else if (normalizedSectionTitle.startsWith(normalizedQuery)) {
      score += 108;
    } else if (normalizedSectionTitle.includes(normalizedQuery)) {
      score += 68;
    }

    if (normalizedPageTitle.startsWith(normalizedQuery)) {
      score += 60;
    } else if (normalizedPageTitle.includes(normalizedQuery)) {
      score += 36;
    }

    if (normalizedPath.includes(normalizedQuery)) {
      score += 20;
    }
  }

  if (matchedTokens === tokens.length) {
    score += 28;
  }

  const ownTitleTier = getSearchTextMatchTier(entry.kind === 'section' ? entry.sectionTitle ?? '' : entry.pageTitle, query);
  const excerptQueryIndex = normalizedQuery.length > 0 ? normalizedExcerpt.indexOf(normalizedQuery) : -1;
  if (entry.kind === 'section' && ownTitleTier === 0 && excerptQueryIndex >= 0 && excerptQueryIndex < 40) {
    score += 60 - excerptQueryIndex * 0.3;
  }

  if (ownTitleTier > 0 && ownTitleTier < 4 && ownTitle !== normalizedQuery) {
    const ownTitleWordCount = ownTitle.split(' ').filter((token) => token.length > 0).length;
    score -= Math.max(ownTitleWordCount - 2, 0) * 10;
  }

  if (entry.kind === 'section' && normalizedSectionTitle.length > 0) {
    score += 6;
  }

  if (entry.kind === 'page') {
    score += 0;
  }

  score -= getGenericSectionPenalty(entry, query);

  if (entry.pathname === '/' && entry.kind === 'section') {
    if (ownTitleTier === 0) {
      score -= 340;
    } else if (ownTitleTier < 3) {
      score -= 380;
    }
  }

  return score - entry.order * 0.01;
}

function getOwnTitleMatchTier(entry: SearchIndexEntry, query: string): number {
  const ownTitle =
    entry.kind === 'section' && entry.sectionTitle
      ? entry.sectionTitle
      : entry.pageTitle;

  return getSearchTextMatchTier(ownTitle, query);
}

function isPrimaryResultBetter(candidate: SearchResult, current: SearchResult, query: string): boolean {
  const candidateOwnTitleTier = getOwnTitleMatchTier(candidate.entry, query);
  const currentOwnTitleTier = getOwnTitleMatchTier(current.entry, query);
  if (candidateOwnTitleTier !== currentOwnTitleTier) {
    return candidateOwnTitleTier > currentOwnTitleTier;
  }

  if (candidateOwnTitleTier > 0 || currentOwnTitleTier > 0) {
    const candidateIsPage = candidate.entry.kind === 'page' ? 1 : 0;
    const currentIsPage = current.entry.kind === 'page' ? 1 : 0;
    if (candidateIsPage !== currentIsPage) {
      return candidateIsPage > currentIsPage;
    }
  }

  const candidatePageTier = getSearchTextMatchTier(candidate.entry.pageTitle, query);
  const currentPageTier = getSearchTextMatchTier(current.entry.pageTitle, query);
  if (candidatePageTier !== currentPageTier) {
    return candidatePageTier > currentPageTier;
  }

  if (candidate.score !== current.score) {
    return candidate.score > current.score;
  }

  if (candidate.entry.order !== current.entry.order) {
    return candidate.entry.order < current.entry.order;
  }

  return candidate.entry.href.localeCompare(current.entry.href) < 0;
}

function hasProminentOwnTitleMatch(entry: SearchIndexEntry, query: string): boolean {
  return getOwnTitleMatchTier(entry, query) >= 3;
}

function compareSearchResults(left: SearchResult, right: SearchResult): number {
  if (right.score !== left.score) {
    return right.score - left.score;
  }

  if (left.entry.order !== right.entry.order) {
    return left.entry.order - right.entry.order;
  }

  return left.entry.href.localeCompare(right.entry.href);
}

export function searchIndex(index: SearchIndexAsset, query: string, limit = DEFAULT_RESULT_LIMIT): SearchResult[] {
  const trimmedQuery = query.trim();
  if (trimmedQuery.length === 0) {
    return buildDefaultResults(index, limit);
  }

  const tokens = tokenizeSearchQuery(trimmedQuery);
  if (tokens.length === 0) {
    return buildDefaultResults(index, limit);
  }

  const ranked = index.items
    .map((entry) => ({
      entry,
      score: scoreEntry(entry, trimmedQuery, tokens),
    }))
    .filter((result) => Number.isFinite(result.score))
    .sort(compareSearchResults)
    .slice(0, Math.max(limit * 4, limit));

  const groupedResults = new Map<string, SearchResult[]>();
  const pathnameOrder: string[] = [];

  for (const result of ranked) {
    if (!groupedResults.has(result.entry.pathname)) {
      groupedResults.set(result.entry.pathname, []);
      pathnameOrder.push(result.entry.pathname);
    }

    groupedResults.get(result.entry.pathname)?.push(result);
  }

  const primary: SearchResult[] = [];
  const overflow: SearchResult[] = [];

  for (const pathname of pathnameOrder) {
    const group = groupedResults.get(pathname);
    if (!group || group.length === 0) {
      continue;
    }

    let primaryResult = group[0];
    for (const candidate of group.slice(1)) {
      if (isPrimaryResultBetter(candidate, primaryResult, trimmedQuery)) {
        primaryResult = candidate;
      }
    }

    primary.push(primaryResult);

    for (const result of group) {
      if (result.entry.href === primaryResult.entry.href) {
        continue;
      }

      if (hasProminentOwnTitleMatch(result.entry, trimmedQuery)) {
        overflow.push(result);
      }
    }
  }

  primary.sort(compareSearchResults);
  overflow.sort(compareSearchResults);

  return [...primary, ...overflow].slice(0, limit);
}
