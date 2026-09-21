import { describe, expect, it } from 'vitest';
import {
  buildSearchHighlightSegments,
  buildSearchResultBreadcrumbs,
  buildSearchResultSnippet,
} from './search-dialog-model';
import type { SearchIndexEntry } from './search-index';

const pageEntry: SearchIndexEntry = {
  body: 'Install ctx with the official installer.',
  excerpt: 'Install ctx with the official installer.',
  groupLabel: 'Getting started',
  href: '/getting-started/install-and-launch',
  kind: 'page',
  keywords: ['Install and Launch'],
  order: 1,
  pageTitle: 'Install and Launch',
  pathname: '/getting-started/install-and-launch',
  sectionTitle: null,
  tabLabel: 'Docs',
  title: 'Install and Launch',
};

const sectionEntry: SearchIndexEntry = {
  ...pageEntry,
  href: '/getting-started/install-and-launch#healthy-first-run-signals',
  kind: 'section',
  sectionTitle: 'Healthy first run signals',
  title: 'Healthy first run signals',
};

describe('buildSearchResultBreadcrumbs', () => {
  it('uses group label then page title for page results', () => {
    expect(buildSearchResultBreadcrumbs(pageEntry)).toEqual(['Getting started', 'Install and Launch']);
  });

  it('uses page title then section title for section results', () => {
    expect(buildSearchResultBreadcrumbs(sectionEntry)).toEqual(['Install and Launch', 'Healthy first run signals']);
  });
});

describe('buildSearchResultSnippet', () => {
  it('prefixes the title when the excerpt starts later in the sentence', () => {
    expect(
      buildSearchResultSnippet({
        ...sectionEntry,
        excerpt: 'Open the app and confirm the harness settings.',
      }),
    ).toBe('Healthy first run signals Open the app and confirm the harness settings.');
  });

  it('does not duplicate the title when the excerpt already begins with it', () => {
    expect(
      buildSearchResultSnippet({
        ...sectionEntry,
        excerpt: 'Healthy first run signals Open the app and confirm the harness settings.',
      }),
    ).toBe('Healthy first run signals Open the app and confirm the harness settings.');
  });
});

describe('buildSearchHighlightSegments', () => {
  it('splits text into matched and unmatched segments', () => {
    expect(buildSearchHighlightSegments('Install and Launch', 'install')).toEqual([
      { matched: true, text: 'Install' },
      { matched: false, text: ' and Launch' },
    ]);
  });

  it('keeps highlights to the exact query tokens visible in the dialog', () => {
    expect(buildSearchHighlightSegments('Connect a Provider', 'providers')).toEqual([
      { matched: false, text: 'Connect a Provider' },
    ]);
  });
});
