import React, { useEffect, useState } from 'react';
import type { Heading } from '../types';
import { TocIcon } from './Icons';

void React;

interface TableOfContentsProps {
  headings: Heading[];
}

export function TableOfContents({ headings }: TableOfContentsProps) {
  const [activeId, setActiveId] = useState<string | null>(headings[0]?.id ?? null);

  useEffect(() => {
    if (headings.length === 0 || typeof window === 'undefined') {
      setActiveId(null);
      return;
    }

    const offset = 164;

    const updateActiveHeading = () => {
      let nextActiveId = headings[0]?.id ?? null;

      for (const heading of headings) {
        const element = document.getElementById(heading.id);
        if (element == null) {
          continue;
        }

        if (element.getBoundingClientRect().top - offset <= 0) {
          nextActiveId = heading.id;
        } else {
          break;
        }
      }

      setActiveId(nextActiveId);
    };

    updateActiveHeading();
    window.addEventListener('hashchange', updateActiveHeading);
    window.addEventListener('resize', updateActiveHeading);
    window.addEventListener('scroll', updateActiveHeading, { passive: true });

    return () => {
      window.removeEventListener('hashchange', updateActiveHeading);
      window.removeEventListener('resize', updateActiveHeading);
      window.removeEventListener('scroll', updateActiveHeading);
    };
  }, [headings]);

  if (headings.length === 0) {
    return <aside className="toc-shell" />;
  }

  return (
    <aside className="toc-shell">
      <div className="toc-panel">
        <div className="toc-title">
          <TocIcon className="toc-title-icon" />
          <span>On this page</span>
        </div>
        <ol className="toc-list">
          {headings.map((heading) => (
            <li key={heading.id}>
              <a
                className={`toc-link depth-${heading.depth}${heading.id === activeId ? ' is-active' : ''}`}
                href={`#${heading.id}`}
              >
                {heading.text}
              </a>
            </li>
          ))}
        </ol>
      </div>
    </aside>
  );
}
