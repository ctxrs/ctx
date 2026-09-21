import { describe, expect, it } from 'vitest';
import { getFooterAwareSidebarScrollTop } from './footer-overlap';

describe('getFooterAwareSidebarScrollTop', () => {
  it('returns zero when the footer is not affecting the sidebar', () => {
    expect(
      getFooterAwareSidebarScrollTop({
        footerHeight: 540,
        footerOverlap: 0,
        maxScroll: 895,
      }),
    ).toBe(0);
  });

  it('scales sidebar scroll by footer overlap progress', () => {
    expect(
      getFooterAwareSidebarScrollTop({
        footerHeight: 540,
        footerOverlap: 270,
        maxScroll: 895,
      }),
    ).toBe(448);
  });

  it('caps the sidebar scroll at the maximum range', () => {
    expect(
      getFooterAwareSidebarScrollTop({
        footerHeight: 540,
        footerOverlap: 620,
        maxScroll: 895,
      }),
    ).toBe(895);
  });
});
