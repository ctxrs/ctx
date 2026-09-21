import { afterEach, describe, expect, it, vi } from 'vitest';
import { loadSearchIndex, resetSearchIndexCacheForTests } from './search-client';
import { SEARCH_INDEX_VERSION } from './search-index';

afterEach(() => {
  resetSearchIndexCacheForTests();
});

describe('loadSearchIndex', () => {
  it('loads and caches the same-origin search asset', async () => {
    const fetchMock = vi.fn(async () =>
      new Response(
        JSON.stringify({
          items: [],
          version: SEARCH_INDEX_VERSION,
        }),
        {
          headers: {
            'Content-Type': 'application/json',
          },
          status: 200,
        },
      ),
    );

    const firstLoad = await loadSearchIndex(fetchMock as typeof fetch);
    const secondLoad = await loadSearchIndex(fetchMock as typeof fetch);

    expect(firstLoad.version).toBe(SEARCH_INDEX_VERSION);
    expect(secondLoad).toBe(firstLoad);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it('clears the cache after a failed fetch so the next open can retry', async () => {
    const failingFetch = vi
      .fn<typeof fetch>()
      .mockRejectedValueOnce(new Error('boom'))
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            items: [],
            version: SEARCH_INDEX_VERSION,
          }),
          {
            headers: {
              'Content-Type': 'application/json',
            },
            status: 200,
          },
        ),
      );

    await expect(loadSearchIndex(failingFetch)).rejects.toThrow('boom');

    const retry = await loadSearchIndex(failingFetch);

    expect(retry.version).toBe(SEARCH_INDEX_VERSION);
    expect(failingFetch).toHaveBeenCalledTimes(2);
  });
});
