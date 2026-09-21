export interface AnchorNavigationIntent {
  download: boolean;
  href: string | null;
  target: string | null;
}

export interface ClientNavigationEventLike {
  altKey: boolean;
  button: number;
  ctrlKey: boolean;
  defaultPrevented: boolean;
  metaKey: boolean;
  shiftKey: boolean;
}

export interface ClientNavigationTarget {
  hash: string;
  href: string;
  pathname: string;
  search: string;
}

export function getClientNavigationTarget(
  currentHref: string,
  anchor: AnchorNavigationIntent,
  event: ClientNavigationEventLike,
): ClientNavigationTarget | null {
  if (
    event.defaultPrevented ||
    event.button !== 0 ||
    event.altKey ||
    event.ctrlKey ||
    event.metaKey ||
    event.shiftKey
  ) {
    return null;
  }

  if (!anchor.href || anchor.download) {
    return null;
  }

  if (anchor.target && anchor.target !== '_self') {
    return null;
  }

  const currentUrl = new URL(currentHref);
  const targetUrl = new URL(anchor.href, currentUrl);

  if (!['http:', 'https:'].includes(targetUrl.protocol) || targetUrl.origin !== currentUrl.origin) {
    return null;
  }

  if (targetUrl.pathname === currentUrl.pathname && targetUrl.search === currentUrl.search) {
    return null;
  }

  return {
    hash: targetUrl.hash,
    href: `${targetUrl.pathname}${targetUrl.search}${targetUrl.hash}`,
    pathname: targetUrl.pathname,
    search: targetUrl.search,
  };
}
