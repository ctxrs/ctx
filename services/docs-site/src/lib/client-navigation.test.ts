import { describe, expect, it } from 'vitest';
import { getClientNavigationTarget } from './client-navigation';

const baseEvent = {
  altKey: false,
  button: 0,
  ctrlKey: false,
  defaultPrevented: false,
  metaKey: false,
  shiftKey: false,
};

describe('client navigation helpers', () => {
  it('returns a target for same-origin internal links', () => {
    expect(
      getClientNavigationTarget(
        'https://ctx.rs/getting-started/install-and-launch',
        { download: false, href: '/concepts/how-it-works', target: null },
        baseEvent,
      ),
    ).toEqual({
      hash: '',
      href: '/concepts/how-it-works',
      pathname: '/concepts/how-it-works',
      search: '',
    });
  });

  it('ignores external links', () => {
    expect(
      getClientNavigationTarget(
        'https://ctx.rs/',
        { download: false, href: 'https://github.com/ctxrs/ctx', target: null },
        baseEvent,
      ),
    ).toBeNull();
  });

  it('ignores modifier-assisted clicks', () => {
    expect(
      getClientNavigationTarget(
        'https://ctx.rs/',
        { download: false, href: '/concepts/how-it-works', target: null },
        { ...baseEvent, metaKey: true },
      ),
    ).toBeNull();
  });

  it('ignores links that request a new browsing context', () => {
    expect(
      getClientNavigationTarget(
        'https://ctx.rs/',
        { download: false, href: '/concepts/how-it-works', target: '_blank' },
        baseEvent,
      ),
    ).toBeNull();
  });

  it('leaves same-page hash jumps to the browser', () => {
    expect(
      getClientNavigationTarget(
        'https://ctx.rs/blog/the-fermai-paradox',
        { download: false, href: '#the-bottleneck-moved', target: null },
        baseEvent,
      ),
    ).toBeNull();
  });
});
