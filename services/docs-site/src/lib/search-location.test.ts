import { describe, expect, it } from 'vitest';
import { getSearchQueryFromHref, withSearchQuery, withoutSearchQuery } from './search-location';

describe('withSearchQuery', () => {
  it('adds the search query before a hash fragment', () => {
    expect(withSearchQuery('/getting-started/install-and-launch#healthy-first-run-signals', 'healthy first run')).toBe(
      '/getting-started/install-and-launch?search=healthy+first+run#healthy-first-run-signals',
    );
  });

  it('preserves existing params when adding the search query', () => {
    expect(withSearchQuery('/docs?page=2#install', 'install')).toBe('/docs?page=2&search=install#install');
  });
});

describe('withoutSearchQuery', () => {
  it('removes only the search param', () => {
    expect(withoutSearchQuery('/docs?page=2&search=install#install')).toBe('/docs?page=2#install');
  });
});

describe('getSearchQueryFromHref', () => {
  it('returns the current search param value', () => {
    expect(getSearchQueryFromHref('/docs?page=2&search=healthy+first+run#install')).toBe('healthy first run');
  });
});
