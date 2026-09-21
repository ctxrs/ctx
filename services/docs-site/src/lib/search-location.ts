const SEARCH_URL_BASE = 'https://ctx.rs';
const SEARCH_PARAM_KEY = 'search';

function toUrl(href: string): URL {
  return new URL(href, SEARCH_URL_BASE);
}

function toRelativeHref(url: URL): string {
  return `${url.pathname}${url.search}${url.hash}`;
}

export function getSearchQueryFromHref(href: string): string {
  return toUrl(href).searchParams.get(SEARCH_PARAM_KEY) ?? '';
}

export function withSearchQuery(href: string, query: string): string {
  const url = toUrl(href);
  const trimmedQuery = query.trim();

  if (trimmedQuery.length === 0) {
    url.searchParams.delete(SEARCH_PARAM_KEY);
  } else {
    url.searchParams.set(SEARCH_PARAM_KEY, trimmedQuery);
  }

  return toRelativeHref(url);
}

export function withoutSearchQuery(href: string): string {
  const url = toUrl(href);
  url.searchParams.delete(SEARCH_PARAM_KEY);
  return toRelativeHref(url);
}
