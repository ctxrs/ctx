export interface Heading {
  depth: 2 | 3;
  id: string;
  text: string;
}

export interface PageRecord {
  description: string;
  descriptionHtml: string;
  groupLabel: string | null;
  headings: Heading[];
  html: string;
  href: string;
  index: number;
  pathname: string;
  sidebarTitle: string;
  slug: string;
  sourcePath: string;
  tabKey: string;
  tabLabel: string;
  title: string;
}

export interface NavGroup {
  label: string;
  pages: string[];
}

export interface NavTab {
  external: boolean;
  groups: NavGroup[];
  href: string;
  key: string;
  label: string;
}

export interface SiteTheme {
  dark: string;
  defaultAppearance: 'dark' | 'light';
  favicon: string;
  light: string;
  logoDark: string;
  logoLight: string;
  primary: string;
}

export interface FooterLinkItem {
  href: string;
  label: string;
}

export interface FooterLinkGroup {
  header: string;
  items: FooterLinkItem[];
}

export interface FooterSocials {
  github?: string;
  slack?: string;
  x?: string;
}

export interface SiteFooterConfig {
  links: FooterLinkGroup[];
  socials: FooterSocials;
}

export interface SearchRecordMetadata {
  breadcrumbs: string[];
  hash: string | null;
  icon: string;
  openapi: string;
  title: string;
}

export interface SearchRecord {
  content: string;
  header: string;
  metadata: SearchRecordMetadata;
  order: number;
  page: string;
}

export interface SearchIndexPayload {
  records: SearchRecord[];
}

export interface SiteData {
  defaultDescription: string;
  footer: SiteFooterConfig | null;
  pageOrder: string[];
  pages: Record<string, PageRecord>;
  redirects: Record<string, string>;
  siteName: string;
  tabs: NavTab[];
  theme: SiteTheme;
}

export interface PageGroup {
  label: string;
  pages: PageRecord[];
}
