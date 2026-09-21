interface FooterAwareSidebarScrollTopOptions {
  footerHeight: number;
  footerOverlap: number;
  maxScroll: number;
}

export function getFooterAwareSidebarScrollTop({
  footerHeight,
  footerOverlap,
  maxScroll,
}: FooterAwareSidebarScrollTopOptions): number {
  if (footerHeight <= 0 || footerOverlap <= 0 || maxScroll <= 0) {
    return 0;
  }

  const progress = Math.min(1, footerOverlap / footerHeight);
  return Math.round(maxScroll * progress);
}
