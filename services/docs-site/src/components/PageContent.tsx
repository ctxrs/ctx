import React, { useEffect, useRef } from 'react';
import { copyText } from '../lib/clipboard';

void React;

interface PageContentProps {
  html: string;
  pathname: string;
}

export function PageContent({ html, pathname }: PageContentProps) {
  const rootRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const root = rootRef.current;
    if (!root) {
      return;
    }

    const resetTimers = new Map<HTMLButtonElement, number>();

    const handleClick = (event: MouseEvent) => {
      const target = event.target;
      if (!(target instanceof Element)) {
        return;
      }

      const copyButton = target.closest<HTMLButtonElement>('[data-copy-code]');
      if (!copyButton) {
        return;
      }

      const codeBlock = copyButton.closest<HTMLElement>('.ctx-code-block');
      const encodedCode = codeBlock?.dataset.code;
      if (!encodedCode) {
        return;
      }

      void copyText(decodeURIComponent(encodedCode)).then(() => {
        copyButton.dataset.tooltip = 'Copied!';
        copyButton.dataset.copied = 'true';

        const existingTimer = resetTimers.get(copyButton);
        if (existingTimer !== undefined) {
          window.clearTimeout(existingTimer);
        }

        const nextTimer = window.setTimeout(() => {
          copyButton.dataset.tooltip = 'Copy';
          delete copyButton.dataset.copied;
          resetTimers.delete(copyButton);
        }, 1200);

        resetTimers.set(copyButton, nextTimer);
      });
    };

    root.addEventListener('click', handleClick);

    return () => {
      root.removeEventListener('click', handleClick);
      for (const timer of resetTimers.values()) {
        window.clearTimeout(timer);
      }
      resetTimers.clear();
    };
  }, [pathname]);

  return (
    <div
      className="page-content"
      data-pathname={pathname}
      ref={rootRef}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
