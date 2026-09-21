import { afterEach, describe, expect, it, vi } from 'vitest';

const client = vi.hoisted(() => ({
  createRoot: vi.fn(() => ({ render: vi.fn() })),
  hydrateRoot: vi.fn(),
}));

vi.mock('react-dom/client', () => client);
vi.mock('./App', () => ({ App: () => null }));
vi.mock('./lib/brand-fonts', () => ({
  loadBrandFonts: () => Promise.resolve(),
  revealBrandFontGate: () => {},
}));

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
  vi.clearAllMocks();
});

describe('site bootstrap', () => {
  it.each([
    [0, 'renders into the empty dev shell'],
    [1, 'hydrates prerendered pages'],
  ])('%s: %s', async (children) => {
    vi.stubGlobal('document', {
      getElementById: () => ({ childElementCount: children }),
    });
    vi.stubGlobal('window', { location: { pathname: '/' } });

    await import('./main');

    expect(client.createRoot).toHaveBeenCalledTimes(children === 0 ? 1 : 0);
    expect(client.hydrateRoot).toHaveBeenCalledTimes(children === 0 ? 0 : 1);
  });
});
