import { createElement } from 'react';
import { renderToString } from 'react-dom/server';
import { App } from './App';
import { buildRouteMetadata, renderRouteHeadHtml } from './lib/metadata';
import { getResolvedPathname } from './lib/site-data';

export interface RenderedRoute {
  canonicalUrl: string;
  description: string;
  headHtml: string;
  html: string;
  title: string;
}

export function renderRoute(pathname: string): RenderedRoute {
  const resolvedPathname = getResolvedPathname(pathname);
  const metadata = buildRouteMetadata(resolvedPathname);

  return {
    canonicalUrl: metadata.canonicalUrl,
    description: metadata.description,
    headHtml: renderRouteHeadHtml(metadata),
    html: renderToString(createElement(App, { pathname: resolvedPathname })),
    title: metadata.title,
  };
}
