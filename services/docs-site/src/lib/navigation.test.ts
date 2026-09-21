import { describe, expect, it } from 'vitest';
import { getActiveTab, getAdjacentPages, getPageGroups, resolvePathname } from './navigation';
import type { SiteData } from '../types';

const fixture: SiteData = {
  defaultDescription: 'fixture',
  footer: null,
  pageOrder: ['/', '/guide', '/blog/post'],
  pages: {
    '/': {
      description: '',
      descriptionHtml: '',
      groupLabel: 'Docs',
      headings: [],
      html: '<p>Home</p>',
      href: '/',
      index: 0,
      pathname: '/',
      slug: 'index',
      sourcePath: 'docs-content/index.mdx',
      tabKey: 'docs',
      tabLabel: 'Docs',
      title: 'Overview',
    },
    '/guide': {
      description: '',
      descriptionHtml: '',
      groupLabel: 'Docs',
      headings: [],
      html: '<p>Guide</p>',
      href: '/guide',
      index: 1,
      pathname: '/guide',
      slug: 'guide',
      sourcePath: 'docs-content/guide.mdx',
      tabKey: 'docs',
      tabLabel: 'Docs',
      title: 'Guide',
    },
    '/blog/post': {
      description: '',
      descriptionHtml: '',
      groupLabel: 'Blog',
      headings: [],
      html: '<p>Blog</p>',
      href: '/blog/post',
      index: 2,
      pathname: '/blog/post',
      slug: 'blog/post',
      sourcePath: 'docs-content/blog/post.mdx',
      tabKey: 'blog',
      tabLabel: 'Blog',
      title: 'Post',
    },
  },
  redirects: {
    '/blog': '/blog/post',
  },
  siteName: 'ctx Docs',
  tabs: [
    {
      external: false,
      groups: [{ label: 'Docs', pages: ['/', '/guide'] }],
      href: '/',
      key: 'docs',
      label: 'Docs',
    },
    {
      external: false,
      groups: [{ label: 'Blog', pages: ['/blog/post'] }],
      href: '/blog/post',
      key: 'blog',
      label: 'Blog',
    },
  ],
  theme: {
    dark: '#166534',
    defaultAppearance: 'dark',
    favicon: '/favicon.ico',
    light: '#33FF33',
    logoDark: '/favicon.ico',
    logoLight: '/favicon.ico',
    primary: '#15803D',
  },
};

describe('navigation helpers', () => {
  it('resolves redirect aliases', () => {
    expect(resolvePathname(fixture, '/blog/')).toBe('/blog/post');
  });

  it('returns the active tab from the resolved page', () => {
    expect(getActiveTab(fixture, '/blog').key).toBe('blog');
  });

  it('returns grouped pages for a tab', () => {
    expect(getPageGroups(fixture, 'docs')).toEqual([
      {
        label: 'Docs',
        pages: [fixture.pages['/'], fixture.pages['/guide']],
      },
    ]);
  });

  it('returns previous and next neighbors', () => {
    expect(getAdjacentPages(fixture, '/guide')).toEqual({
      previous: fixture.pages['/'],
      next: fixture.pages['/blog/post'],
    });
  });
});
