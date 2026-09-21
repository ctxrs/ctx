import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { PageFooterNav } from './PageFooterNav';

describe('PageFooterNav', () => {
  it('prefers sidebar titles for neighbor labels', () => {
    const markup = renderToStaticMarkup(
      React.createElement(PageFooterNav, {
        neighbors: {
          previous: {
            description: '',
            descriptionHtml: '',
            groupLabel: 'Getting started',
            headings: [],
            html: '',
            href: '/',
            index: 0,
            pathname: '/',
            sidebarTitle: 'Overview',
            slug: 'index',
            sourcePath: 'docs-content/index.mdx',
            tabKey: 'docs',
            tabLabel: 'Docs',
            title: 'ctx | Coding agent history search',
          },
          next: {
            description: '',
            descriptionHtml: '',
            groupLabel: 'Getting started',
            headings: [],
            html: '',
            href: '/getting-started/connect-provider',
            index: 2,
            pathname: '/getting-started/connect-provider',
            sidebarTitle: 'Connect a Provider',
            slug: 'connect-provider',
            sourcePath: 'docs-content/getting-started/connect-provider.mdx',
            tabKey: 'docs',
            tabLabel: 'Docs',
            title: 'Connect a Provider',
          },
        },
      }),
    );

    expect(markup).toContain('Overview');
    expect(markup).toContain('Connect a Provider');
    expect(markup).not.toContain('ctx | Coding agent history search');
  });
});
