function isExternalUrl(value: string): boolean {
  return /^[a-z][a-z0-9+.-]*:/i.test(value) || value.startsWith('//');
}

function hasFileExtension(pathname: string): boolean {
  return /\.[a-z0-9]+$/i.test(pathname);
}

export function toCanonicalPath(pathname: string): string {
  if (pathname === '/' || hasFileExtension(pathname)) {
    return pathname;
  }

  return toRouteHref(pathname);
}

export function toRouteHref(pathname: string): string {
  if (pathname === '/') {
    return '/';
  }

  return pathname.endsWith('/') ? pathname : `${pathname}/`;
}

export function toSiteHref(href: string): string {
  if (!href || isExternalUrl(href) || href.startsWith('#')) {
    return href;
  }

  const [beforeHash, hash = ''] = href.split('#', 2);
  const [pathname, search = ''] = beforeHash.split('?', 2);

  if (!pathname.startsWith('/')) {
    return href;
  }

  const normalizedPathname = toCanonicalPath(pathname);
  const searchSuffix = search ? `?${search}` : '';
  const hashSuffix = hash ? `#${hash}` : '';
  return `${normalizedPathname}${searchSuffix}${hashSuffix}`;
}
