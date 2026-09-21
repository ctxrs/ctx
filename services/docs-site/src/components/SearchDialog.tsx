import React, { Fragment, useEffect, useId, useRef, useState } from 'react';
import './SearchDialog.css';
import {
  buildSearchHighlightSegments,
  buildSearchResultBreadcrumbs,
  buildSearchResultSnippet,
} from '../lib/search-dialog-model';
import { getSearchQueryFromHref, withSearchQuery, withoutSearchQuery } from '../lib/search-location';
import { loadSearchIndex } from '../lib/search-client';
import { searchIndex as querySearchIndex, type SearchIndexAsset, type SearchResult } from '../lib/search-index';
import { SearchIcon } from './Icons';

void React;

interface SearchDialogProps {
  isOpen: boolean;
  onClose: () => void;
  onNavigate: (href: string) => void;
}

type SearchStatus = 'idle' | 'loading' | 'ready' | 'error';

interface SearchDocumentIconProps {
  className?: string;
}

function SearchDocumentIcon({ className }: SearchDocumentIconProps) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      fill="none"
      viewBox="0 0 16 16"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M5.111 6H6.889" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
      <path d="M5.111 8.667H10.889" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
      <path d="M5.111 11.333H10.889" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
      <path
        d="M2.444 12.667V3.333c0-.982.796-1.777 1.778-1.777h4.965c.236 0 .462.093.628.26l3.48 3.479c.167.167.26.393.26.628v6.744c0 .982-.796 1.778-1.778 1.778H4.222c-.982 0-1.778-.796-1.778-1.778Z"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.5"
      />
      <path
        d="M13.476 5.556h-3.031a.889.889 0 0 1-.889-.889V1.646"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.5"
      />
    </svg>
  );
}

function SearchHashIcon({ className }: SearchDocumentIconProps) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      fill="none"
      viewBox="0 0 16 16"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M3.333 5.556h10.223" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
      <path d="M2.444 10.444h10.222" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
      <path d="M6.785 2.444 4.701 13.555" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
      <path d="M11.299 2.444 9.215 13.555" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
    </svg>
  );
}

function SearchChevronRightIcon({ className }: SearchDocumentIconProps) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      fill="none"
      viewBox="0 0 24 24"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="m9 18 6-6-6-6" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2" />
    </svg>
  );
}

function SearchBreadcrumbChevron({ className }: SearchDocumentIconProps) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      fill="currentColor"
      viewBox="0 0 24 24"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M13.172 12 8.222 7.05l1.414-1.414L16 12l-6.364 6.364-1.414-1.414L13.172 12Z" />
    </svg>
  );
}

function renderHighlightedText(text: string, query: string, keyPrefix: string): React.ReactNode {
  const segments = buildSearchHighlightSegments(text, query);
  if (segments.length === 0) {
    return text;
  }

  return segments.map((segment, segmentIndex) => {
    if (!segment.matched) {
      return <Fragment key={`${keyPrefix}-${segmentIndex}`}>{segment.text}</Fragment>;
    }

    return (
      <mark key={`${keyPrefix}-${segmentIndex}`}>
        <b>{segment.text}</b>
      </mark>
    );
  });
}

export function SearchDialog({ isOpen, onClose, onNavigate }: SearchDialogProps) {
  const resultsListId = useId();
  const inputRef = useRef<HTMLInputElement | null>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);
  const shouldRestoreFocusRef = useRef(true);
  const [query, setQuery] = useState('');
  const [status, setStatus] = useState<SearchStatus>('idle');
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [searchAsset, setSearchAsset] = useState<SearchIndexAsset | null>(null);
  const [activeIndex, setActiveIndex] = useState(0);

  const trimmedQuery = query.trim();
  const results = searchAsset && trimmedQuery.length > 0 ? querySearchIndex(searchAsset, trimmedQuery) : [];
  const activeResult = results[activeIndex] ?? null;
  const hasResultsPanel = trimmedQuery.length > 0 || status === 'error';

  useEffect(() => {
    if (!isOpen) {
      setQuery('');
      setActiveIndex(0);
      setErrorMessage(null);
      return;
    }

    if (typeof document !== 'undefined') {
      const activeElement = document.activeElement;
      restoreFocusRef.current = activeElement instanceof HTMLElement ? activeElement : null;
    }

    shouldRestoreFocusRef.current = true;

    if (typeof window !== 'undefined') {
      setQuery(getSearchQueryFromHref(window.location.href));
    }

    window.requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.select();
    });
  }, [isOpen]);

  useEffect(() => {
    if (typeof document === 'undefined') {
      return;
    }

    if (!isOpen) {
      if (shouldRestoreFocusRef.current) {
        restoreFocusRef.current?.focus();
      }

      restoreFocusRef.current = null;
      return;
    }

    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    return () => {
      document.body.style.overflow = previousOverflow;
    };
  }, [isOpen]);

  useEffect(() => {
    if (!isOpen || typeof window === 'undefined') {
      return;
    }

    const nextHref = withSearchQuery(window.location.href, query);
    const currentHref = `${window.location.pathname}${window.location.search}${window.location.hash}`;
    if (nextHref !== currentHref) {
      window.history.replaceState(null, '', nextHref);
    }
  }, [isOpen, query]);

  useEffect(() => {
    if (isOpen || typeof window === 'undefined') {
      return;
    }

    const nextHref = withoutSearchQuery(window.location.href);
    const currentHref = `${window.location.pathname}${window.location.search}${window.location.hash}`;
    if (nextHref !== currentHref) {
      window.history.replaceState(null, '', nextHref);
    }
  }, [isOpen]);

  useEffect(() => {
    if (!isOpen) {
      return undefined;
    }

    function onKeyDown(event: KeyboardEvent): void {
      if (event.key === 'Escape') {
        event.preventDefault();
        onClose();
      }
    }

    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [isOpen, onClose]);

  useEffect(() => {
    if (!isOpen || searchAsset) {
      if (searchAsset) {
        setStatus('ready');
      }
      return;
    }

    let cancelled = false;
    setStatus('loading');
    setErrorMessage(null);

    void loadSearchIndex()
      .then((nextAsset) => {
        if (cancelled) {
          return;
        }

        setSearchAsset(nextAsset);
        setStatus('ready');
      })
      .catch((error: unknown) => {
        if (cancelled) {
          return;
        }

        setStatus('error');
        setErrorMessage(error instanceof Error ? error.message : 'Search index failed to load.');
      });

    return () => {
      cancelled = true;
    };
  }, [isOpen, searchAsset]);

  useEffect(() => {
    setActiveIndex(0);
  }, [query, status]);

  function openResult(result: SearchResult): void {
    shouldRestoreFocusRef.current = false;
    onNavigate(withSearchQuery(result.entry.href, query));
  }

  function handleQueryKeyDown(event: React.KeyboardEvent<HTMLInputElement>): void {
    if (event.key === 'ArrowDown') {
      if (results.length === 0) {
        return;
      }

      event.preventDefault();
      setActiveIndex((currentIndex) => (currentIndex + 1) % results.length);
      return;
    }

    if (event.key === 'ArrowUp') {
      if (results.length === 0) {
        return;
      }

      event.preventDefault();
      setActiveIndex((currentIndex) => (currentIndex - 1 + results.length) % results.length);
      return;
    }

    if (event.key === 'Enter' && activeResult) {
      event.preventDefault();
      openResult(activeResult);
    }
  }

  if (!isOpen) {
    return null;
  }

  return (
    <div className="search-dialog-backdrop" role="presentation" onClick={onClose}>
      <div
        aria-label="Search docs"
        aria-modal="true"
        className="search-dialog"
        onClick={(event) => event.stopPropagation()}
        role="dialog"
      >
        <div className="search-dialog-header">
          <div className="search-dialog-input-row">
            <SearchIcon className="search-dialog-input-icon" />
            <input
              aria-activedescendant={activeResult ? `search-result-${activeIndex}` : undefined}
              aria-autocomplete="list"
              aria-controls={results.length > 0 ? resultsListId : undefined}
              aria-expanded={hasResultsPanel}
              aria-label="Search docs"
              autoCapitalize="none"
              autoCorrect="off"
              className="search-dialog-input"
              onChange={(event) => setQuery(event.target.value)}
              onKeyDown={handleQueryKeyDown}
              placeholder="Search..."
              ref={inputRef}
              role="combobox"
              spellCheck={false}
              type="search"
              value={query}
            />
            <button
              aria-label="Close search"
              className="search-dialog-close"
              onClick={onClose}
              type="button"
            >
              ESC
            </button>
          </div>
        </div>
        {hasResultsPanel ? (
          <div className="search-results-panel">
            {status === 'loading' ? (
              <div className="search-dialog-status">Loading…</div>
            ) : null}
            {status === 'error' ? (
              <div className="search-dialog-status is-error">
                Search is unavailable right now.
                {errorMessage ? <span>{errorMessage}</span> : null}
              </div>
            ) : null}
            {status === 'ready' && trimmedQuery.length > 0 && results.length === 0 ? (
              <div className="search-dialog-status">No matches found.</div>
            ) : null}
            {status === 'ready' && results.length > 0 ? (
              <ul className="search-results" id={resultsListId} role="listbox">
                {results.map((result, resultIndex) => {
                  const isActive = resultIndex === activeIndex;
                  const breadcrumbs = buildSearchResultBreadcrumbs(result.entry);

                  return (
                    <li className="search-result-item" key={result.entry.href} role="presentation">
                      <button
                        aria-selected={isActive}
                        className={`search-result-button${isActive ? ' is-active' : ''}`}
                        id={`search-result-${resultIndex}`}
                        onClick={() => openResult(result)}
                        onMouseEnter={() => {
                          if (!isActive) {
                            setActiveIndex(resultIndex);
                          }
                        }}
                        role="option"
                        type="button"
                      >
                        <span className="search-result-icon-wrap" aria-hidden="true">
                          {result.entry.kind === 'page' ? (
                            <SearchDocumentIcon className="search-result-icon" />
                          ) : (
                            <SearchHashIcon className="search-result-icon" />
                          )}
                        </span>
                        <span className="search-result-body">
                          <span className="search-result-breadcrumbs">
                            {breadcrumbs.map((breadcrumb, breadcrumbIndex) => (
                              <Fragment key={`${result.entry.href}-crumb-${breadcrumb}`}>
                                {breadcrumbIndex > 0 ? (
                                  <SearchBreadcrumbChevron className="search-result-breadcrumb-separator" />
                                ) : null}
                                <span className="search-result-breadcrumb">
                                  {renderHighlightedText(
                                    breadcrumb,
                                    query,
                                    `${result.entry.href}-crumb-${breadcrumbIndex}`,
                                  )}
                                </span>
                              </Fragment>
                            ))}
                          </span>
                          <span className="search-result-title-row">
                            <span className="search-result-title">
                              {renderHighlightedText(
                                result.entry.title,
                                query,
                                `${result.entry.href}-title`,
                              )}
                            </span>
                          </span>
                          <span className="search-result-excerpt">
                            {renderHighlightedText(
                              buildSearchResultSnippet(result.entry),
                              query,
                              `${result.entry.href}-excerpt`,
                            )}
                          </span>
                        </span>
                        <SearchChevronRightIcon className="search-result-chevron" />
                      </button>
                    </li>
                  );
                })}
              </ul>
            ) : null}
          </div>
        ) : null}
      </div>
    </div>
  );
}
