import { siteData } from '../generated/site-data';
import type { NavTab, PageGroup, PageRecord, SiteData } from '../types';
import {
  getActiveTab as resolveActiveTab,
  getAdjacentPages as resolveAdjacentPages,
  getPage,
  getPageGroups,
  resolvePathname,
  type PageWithNeighbors,
} from './navigation';

export type { PageWithNeighbors };

export function getSiteData(): SiteData {
  return siteData;
}

export function getResolvedPathname(pathname: string): string {
  return resolvePathname(siteData, pathname);
}

export function getPageForPathname(pathname: string): PageRecord | null {
  return getPage(siteData, pathname);
}

export function getActiveTab(pathname: string): NavTab {
  return resolveActiveTab(siteData, pathname);
}

export function getPageGroupsForTab(tabKey: string): PageGroup[] {
  return getPageGroups(siteData, tabKey);
}

export function getAdjacentPages(pathname: string): PageWithNeighbors | null {
  return resolveAdjacentPages(siteData, pathname);
}
