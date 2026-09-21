import { describe, expect, it } from 'vitest';
import { SEARCH_INDEX_VERSION, searchIndex, type SearchIndexAsset } from './search-index';

const testIndex: SearchIndexAsset = {
  items: [
    {
      body: 'Install ctx with the official installer and run your first local search.',
      excerpt: 'Install ctx with the official installer.',
      groupLabel: 'Getting started',
      href: '/getting-started/install-and-launch',
      kind: 'page',
      keywords: ['Install and Launch', 'Getting started', 'installer'],
      order: 2,
      pageTitle: 'Install and Launch',
      pathname: '/getting-started/install-and-launch',
      sectionTitle: null,
      tabLabel: 'Docs',
      title: 'Install and Launch',
    },
    {
      body: 'Install ctx with the official installer.',
      excerpt: 'Install ctx with the official installer.',
      groupLabel: 'Getting started',
      href: '/getting-started/install-and-launch#before-you-install',
      kind: 'section',
      keywords: ['Install and Launch', 'Getting started', 'Before you install'],
      order: 2.01,
      pageTitle: 'Install and Launch',
      pathname: '/getting-started/install-and-launch',
      sectionTitle: 'Before you install',
      tabLabel: 'Docs',
      title: 'Before you install',
    },
    {
      body: 'Healthy first run means setup completes and provider import works.',
      excerpt: 'Healthy first run means setup completes.',
      groupLabel: 'Getting started',
      href: '/getting-started/install-and-launch#healthy-first-run-signals',
      kind: 'section',
      keywords: ['Install and Launch', 'Getting started', 'Healthy first run signals'],
      order: 2.02,
      pageTitle: 'Install and Launch',
      pathname: '/getting-started/install-and-launch',
      sectionTitle: 'Healthy first run signals',
      tabLabel: 'Docs',
      title: 'Healthy first run signals',
    },
    {
      body: 'Connect your coding agent provider accounts or API keys.',
      excerpt: 'Connect your coding agent provider accounts or API keys.',
      groupLabel: 'Getting started',
      href: '/getting-started/connect-provider',
      kind: 'page',
      keywords: ['Connect a Provider', 'Getting started', 'provider'],
      order: 3,
      pageTitle: 'Connect a Provider',
      pathname: '/getting-started/connect-provider',
      sectionTitle: null,
      tabLabel: 'Docs',
      title: 'Connect a Provider',
    },
    {
      body: 'Small, reviewable tasks are easier to steer and trust.',
      excerpt: 'Small, reviewable tasks are easier to steer and trust.',
      groupLabel: 'Highlights',
      href: '/#good-first-tasks',
      kind: 'section',
      keywords: ['ctx', 'Good first tasks'],
      order: 0.04,
      pageTitle: 'ctx | Coding agent history search',
      pathname: '/',
      sectionTitle: 'Good first tasks',
      tabLabel: 'Docs',
      title: 'Good first tasks',
    },
    {
      body: 'Search results include sessions, events, files, and provider citations.',
      excerpt: 'Search results include sessions, events, files, and provider citations.',
      groupLabel: 'Concepts',
      href: '/concepts/how-it-works#indexing-and-search',
      kind: 'section',
      keywords: ['How it works', 'Indexing and search'],
      order: 9.01,
      pageTitle: 'How it works',
      pathname: '/concepts/how-it-works',
      sectionTitle: 'Indexing and search',
      tabLabel: 'Docs',
      title: 'Indexing and search',
    },
    {
      body: 'Import Search Show Locate Docs.',
      excerpt: 'Import Search Show Locate Docs.',
      groupLabel: 'Concepts',
      href: '/concepts/how-it-works#related-pages',
      kind: 'section',
      keywords: ['How it works', 'Related pages'],
      order: 9.05,
      pageTitle: 'How it works',
      pathname: '/concepts/how-it-works',
      sectionTitle: 'Related pages',
      tabLabel: 'Docs',
      title: 'Related pages',
    },
  ],
  version: SEARCH_INDEX_VERSION,
};

describe('searchIndex', () => {
  it('returns no results when the query is empty', () => {
    const results = searchIndex(testIndex, '');

    expect(results).toEqual([]);
  });

  it('prefers direct title matches over body-only matches', () => {
    const results = searchIndex(testIndex, 'install');

    expect(results[0]?.entry.href).toBe('/getting-started/install-and-launch');
    expect(results.map((result) => result.entry.href)).not.toContain('/getting-started/install-and-launch#before-you-install');
  });

  it('matches terms from keywords and body copy', () => {
    const results = searchIndex(testIndex, 'provider');

    expect(results[0]?.entry.href).toBe('/getting-started/connect-provider');
  });

  it('matches simple plural and singular variants', () => {
    const results = searchIndex(testIndex, 'providers');

    expect(results[0]?.entry.href).toBe('/getting-started/connect-provider');
  });

  it('keeps a same-page section result first when the section title directly matches the query', () => {
    const results = searchIndex(testIndex, 'healthy first run');

    expect(results[0]?.entry.href).toBe('/getting-started/install-and-launch#healthy-first-run-signals');
  });

  it('prefers specific sections over generic related-pages buckets for the same pathname', () => {
    const results = searchIndex(testIndex, 'search');

    expect(results.find((result) => result.entry.pathname === '/concepts/how-it-works')?.entry.href).toBe(
      '/concepts/how-it-works#indexing-and-search',
    );
  });

  it('de-emphasizes homepage anchor sections for broad term matches', () => {
    const results = searchIndex(testIndex, 'search', 6);

    expect(results[0]?.entry.href).toBe('/concepts/how-it-works#indexing-and-search');
    expect(results[results.length - 1]?.entry.href).toBe('/#good-first-tasks');
  });
});
