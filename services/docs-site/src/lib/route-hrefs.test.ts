import { describe, expect, it } from 'vitest';
import { toCanonicalPath, toRouteHref, toSiteHref } from './route-hrefs';

describe('route href helpers', () => {
  it('adds trailing slashes to internal HTML routes', () => {
    expect(toRouteHref('/')).toBe('/');
    expect(toRouteHref('/concepts/how-it-works')).toBe('/concepts/how-it-works/');
    expect(toRouteHref('/concepts/how-it-works/')).toBe('/concepts/how-it-works/');
  });

  it('normalizes internal hrefs but leaves assets and external URLs alone', () => {
    expect(toSiteHref('/concepts/how-it-works')).toBe('/concepts/how-it-works/');
    expect(toSiteHref('/concepts/how-it-works#indexing')).toBe('/concepts/how-it-works/#indexing');
    expect(toSiteHref('/search-index.json')).toBe('/search-index.json');
    expect(toSiteHref('https://github.com/ctxrs/ctx')).toBe('https://github.com/ctxrs/ctx');
  });

  it('adds trailing slashes only to HTML route paths', () => {
    expect(toCanonicalPath('/concepts/how-it-works')).toBe('/concepts/how-it-works/');
    expect(toCanonicalPath('/videos/ctx-homepage-demo-social.png')).toBe(
      '/videos/ctx-homepage-demo-social.png',
    );
    expect(toCanonicalPath('/index.md')).toBe('/index.md');
  });
});
