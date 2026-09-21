import React from 'react';
import type { PageWithNeighbors } from '../lib/site-data';
import { ChevronIcon } from './Icons';

void React;

interface PageFooterNavProps {
  neighbors: PageWithNeighbors | null;
}

export function PageFooterNav({ neighbors }: PageFooterNavProps) {
  if (!neighbors || (!neighbors.previous && !neighbors.next)) {
    return null;
  }

  const previousTitle = neighbors.previous?.sidebarTitle ?? neighbors.previous?.title;
  const nextTitle = neighbors.next?.sidebarTitle ?? neighbors.next?.title;

  return (
    <nav aria-label="Page links" className="footer-nav">
      {neighbors.previous && previousTitle ? (
        <a className="footer-nav-link is-previous" href={neighbors.previous.href}>
          <ChevronIcon className="footer-nav-icon is-previous" />
          <span className="footer-nav-title">{previousTitle}</span>
        </a>
      ) : (
        <span className="footer-nav-spacer" />
      )}
      {neighbors.next && nextTitle ? (
        <a className="footer-nav-link is-next" href={neighbors.next.href}>
          <span className="footer-nav-title">{nextTitle}</span>
          <ChevronIcon className="footer-nav-icon is-next" />
        </a>
      ) : (
        <span className="footer-nav-spacer" />
      )}
    </nav>
  );
}
