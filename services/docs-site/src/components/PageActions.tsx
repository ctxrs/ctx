import React from 'react';
import { ChevronDownIcon, CopyIcon } from './Icons';

void React;

interface PageActionsProps {
  className?: string;
  onCopy: () => void;
}

export function PageActions({ className, onCopy }: PageActionsProps) {
  const rootClassName = className ? `page-actions ${className}` : 'page-actions';

  return (
    <div className={rootClassName}>
      <button
        aria-label="Copy page"
        className="page-actions-copy"
        onClick={onCopy}
        type="button"
      >
        <span className="page-actions-copy-inner">
          <CopyIcon className="page-actions-copy-icon" />
          <span>Copy page</span>
        </span>
      </button>
      <button
        aria-label="More actions"
        className="page-actions-more"
        type="button"
      >
        <ChevronDownIcon className="page-actions-more-icon" />
      </button>
    </div>
  );
}
