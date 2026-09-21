import {
  SEARCH_INDEX_ASSET_PATH,
  SEARCH_INDEX_VERSION,
  type SearchIndexAsset,
  type SearchIndexEntry,
} from './search-index';

type SearchFetch = typeof fetch;

let searchIndexPromise: Promise<SearchIndexAsset> | null = null;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function isSearchIndexEntry(value: unknown): value is SearchIndexEntry {
  if (!isRecord(value)) {
    return false;
  }

  return (
    typeof value.body === 'string' &&
    typeof value.excerpt === 'string' &&
    (value.groupLabel === null || typeof value.groupLabel === 'string') &&
    typeof value.href === 'string' &&
    (value.kind === 'page' || value.kind === 'section') &&
    Array.isArray(value.keywords) &&
    value.keywords.every((keyword) => typeof keyword === 'string') &&
    typeof value.order === 'number' &&
    typeof value.pageTitle === 'string' &&
    typeof value.pathname === 'string' &&
    (value.sectionTitle === null || typeof value.sectionTitle === 'string') &&
    typeof value.tabLabel === 'string' &&
    typeof value.title === 'string'
  );
}

function parseSearchIndexAsset(value: unknown): SearchIndexAsset {
  if (!isRecord(value)) {
    throw new Error('Search index payload is not an object.');
  }

  if (value.version !== SEARCH_INDEX_VERSION) {
    throw new Error('Search index payload version is not supported.');
  }

  if (!Array.isArray(value.items) || !value.items.every(isSearchIndexEntry)) {
    throw new Error('Search index payload items are invalid.');
  }

  return {
    items: value.items,
    version: SEARCH_INDEX_VERSION,
  };
}

export async function loadSearchIndex(fetchImpl: SearchFetch = fetch): Promise<SearchIndexAsset> {
  if (searchIndexPromise) {
    return searchIndexPromise;
  }

  searchIndexPromise = (async () => {
    const response = await fetchImpl(SEARCH_INDEX_ASSET_PATH, {
      headers: {
        Accept: 'application/json',
      },
    });

    if (!response.ok) {
      throw new Error(`Search index request failed with ${response.status}.`);
    }

    const payload = (await response.json()) as unknown;
    return parseSearchIndexAsset(payload);
  })();

  try {
    return await searchIndexPromise;
  } catch (error) {
    searchIndexPromise = null;
    throw error;
  }
}

export function resetSearchIndexCacheForTests(): void {
  searchIndexPromise = null;
}
