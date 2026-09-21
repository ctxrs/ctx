import type { NavTab, PageGroup, PageRecord, SiteData } from '../types';

export interface PageWithNeighbors {
  next: PageRecord | null;
  previous: PageRecord | null;
}

function fallbackTab(site: SiteData): NavTab {
  const firstTab = site.tabs[0];

  if (!firstTab) {
    throw new Error('Missing navigation tabs');
  }

  return firstTab;
}

export function normalizePathname(pathname: string): string {
  if (pathname === '' || pathname === '/') {
    return '/';
  }

  const trimmedPath = pathname.endsWith('/') ? pathname.slice(0, -1) : pathname;
  return trimmedPath.startsWith('/') ? trimmedPath : `/${trimmedPath}`;
}

export function resolvePathname(site: SiteData, pathname: string): string {
  const normalizedPathname = normalizePathname(pathname);
  return site.redirects[normalizedPathname] ?? normalizedPathname;
}

export function getPage(site: SiteData, pathname: string): PageRecord | null {
  const resolvedPathname = resolvePathname(site, pathname);
  return site.pages[resolvedPathname] ?? null;
}

export function getActiveTab(site: SiteData, pathname: string): NavTab {
  const resolvedPathname = resolvePathname(site, pathname);
  const page = site.pages[resolvedPathname];

  if (page) {
    const matchingTab = site.tabs.find((tab) => tab.key === page.tabKey);
    if (matchingTab) {
      return matchingTab;
    }
  }

  const matchingTab = site.tabs.find((tab) => {
    if (tab.external) {
      return false;
    }

    return tab.groups.some((group) =>
      group.pages.some((pagePath) => normalizePathname(pagePath) === resolvedPathname),
    );
  });

  return matchingTab ?? fallbackTab(site);
}

export function getPageGroups(site: SiteData, tabKey: string): PageGroup[] {
  const tab = site.tabs.find((entry) => entry.key === tabKey);
  if (!tab) {
    return [];
  }

  return tab.groups
    .map((group) => ({
      label: group.label,
      pages: group.pages
        .map((pathname) => site.pages[normalizePathname(pathname)])
        .filter((page): page is PageRecord => Boolean(page)),
    }))
    .filter((group) => group.pages.length > 0);
}

export function getAdjacentPages(site: SiteData, pathname: string): PageWithNeighbors | null {
  const resolvedPathname = resolvePathname(site, pathname);
  const pageIndex = site.pageOrder.indexOf(resolvedPathname);

  if (pageIndex === -1) {
    return null;
  }

  const previousPathname = pageIndex > 0 ? site.pageOrder[pageIndex - 1] : undefined;
  const nextPathname =
    pageIndex < site.pageOrder.length - 1 ? site.pageOrder[pageIndex + 1] : undefined;

  return {
    previous: previousPathname ? site.pages[previousPathname] ?? null : null,
    next: nextPathname ? site.pages[nextPathname] ?? null : null,
  };
}
