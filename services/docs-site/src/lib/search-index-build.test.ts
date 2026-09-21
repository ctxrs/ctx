import { describe, expect, it } from 'vitest';
import { buildSearchIndex, extractSearchSections } from './search-index-build';

describe('extractSearchSections', () => {
  it('keeps pre-heading copy separate from heading sections', () => {
    const result = extractSearchSections(`
Intro paragraph before any heading.

## Install

Run the installer first.

### Verify

Make sure the app opens.
`);

    expect(result.preamble).toBe('Intro paragraph before any heading.');
    expect(result.sections).toEqual([
      {
        body: 'Run the installer first.',
        depth: 2,
        id: 'install',
        title: 'Install',
      },
      {
        body: 'Make sure the app opens.',
        depth: 3,
        id: 'verify',
        title: 'Verify',
      },
    ]);
  });

  it('preserves spacing across list items inside a section body', () => {
    const result = extractSearchSections(`
## Before you install

- machine with the repository you want to use
- at least one provider account or API key
- container support if you plan to start sandboxes
`);

    expect(result.sections[0]).toEqual({
      body: 'machine with the repository you want to use at least one provider account or API key container support if you plan to start sandboxes',
      depth: 2,
      id: 'before-you-install',
      title: 'Before you install',
    });
  });
});

describe('buildSearchIndex', () => {
  it('emits page and section entries with same-page anchors', () => {
    const index = buildSearchIndex([
      {
        description: 'Install ctx and verify first-run health.',
        groupLabel: 'Getting started',
        order: 1,
        pathname: '/getting-started/install-and-launch',
        source: `
Install ctx with the official installer.

## First launch checklist

Open the app and confirm the harness settings.
`,
        tabLabel: 'Docs',
        title: 'Install and Launch',
      },
    ]);

    expect(index.items).toHaveLength(2);
    expect(index.items[0]).toMatchObject({
      href: '/getting-started/install-and-launch/',
      kind: 'page',
      pageTitle: 'Install and Launch',
      title: 'Install and Launch',
    });
    expect(index.items[1]).toMatchObject({
      href: '/getting-started/install-and-launch/#first-launch-checklist',
      kind: 'section',
      pageTitle: 'Install and Launch',
      sectionTitle: 'First launch checklist',
      title: 'First launch checklist',
    });
  });
});
