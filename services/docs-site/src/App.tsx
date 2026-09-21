import React, { useEffect, useRef, useState } from 'react';
import { Header } from './components/Header';
import { PageContent } from './components/PageContent';
import { PageActions } from './components/PageActions';
import { PageFooterNav } from './components/PageFooterNav';
import { Sidebar } from './components/Sidebar';
import { SiteFooter } from './components/SiteFooter';
import { TableOfContents } from './components/TableOfContents';
import { copyText } from './lib/clipboard';
import { getClientNavigationTarget } from './lib/client-navigation';
import { getFooterAwareSidebarScrollTop } from './lib/footer-overlap';
import { applyRouteMetadata, buildCanonicalUrl } from './lib/metadata';
import {
  getActiveTab,
  getAdjacentPages,
  getPageForPathname,
  getPageGroupsForTab,
  getResolvedPathname,
  getSiteData,
  type PageWithNeighbors,
} from './lib/site-data';
import type { PageRecord } from './types';

void React;

type SearchDialogLoadStatus = 'idle' | 'loading' | 'ready' | 'error';

interface SearchDialogComponentProps {
  isOpen: boolean;
  onClose: () => void;
  onNavigate: (href: string) => void;
}

type SearchDialogView = (props: SearchDialogComponentProps) => React.ReactNode;

let searchDialogPromise: Promise<SearchDialogView> | null = null;

function loadSearchDialogComponent(): Promise<SearchDialogView> {
  if (searchDialogPromise) {
    return searchDialogPromise;
  }

  searchDialogPromise = import('./components/SearchDialog')
    .then((module) => module.SearchDialog)
    .catch((error: unknown) => {
      searchDialogPromise = null;
      throw error;
    });

  return searchDialogPromise;
}

interface AppProps {
  pathname: string;
}

interface SearchNavigationTarget {
  hash: string;
  href: string;
  pathname: string;
  sameDocument: boolean;
  search: string;
}

const DEFAULT_CLOUD_WAITLIST_ENDPOINT = 'https://api.ctx.rs/functions/v1/cloud-waitlist';
const CLOUD_WAITLIST_ENDPOINT = (
  import.meta.env?.VITE_CTX_WAITLIST_ENDPOINT ??
  (typeof process === 'undefined' ? undefined : process.env.VITE_CTX_WAITLIST_ENDPOINT) ??
  DEFAULT_CLOUD_WAITLIST_ENDPOINT
).trim();

function getSearchNavigationTarget(currentHref: string, href: string): SearchNavigationTarget | null {
  const currentUrl = new URL(currentHref);
  const targetUrl = new URL(href, currentUrl);

  if (!['http:', 'https:'].includes(targetUrl.protocol) || targetUrl.origin !== currentUrl.origin) {
    return null;
  }

  return {
    hash: targetUrl.hash,
    href: `${targetUrl.pathname}${targetUrl.search}${targetUrl.hash}`,
    pathname: targetUrl.pathname,
    sameDocument:
      targetUrl.pathname === currentUrl.pathname && targetUrl.search === currentUrl.search,
    search: targetUrl.search,
  };
}

function copyPageUrl(pathname: string): void {
  if (typeof window === 'undefined') {
    return;
  }

  void copyText(buildCanonicalUrl(pathname));
}

function formDataString(formData: FormData, key: string): string | null {
  const value = formData.get(key);
  if (typeof value !== 'string') {
    return null;
  }
  const normalized = value.trim();
  return normalized.length > 0 ? normalized : null;
}

function setCloudWaitlistStatus(
  form: HTMLFormElement,
  tone: 'error' | 'success' | 'working',
  message: string,
): void {
  const status = form.querySelector<HTMLElement>('[data-cloud-waitlist-status]');
  if (!status) {
    return;
  }
  status.dataset.tone = tone;
  status.textContent = message;
}

function buildCloudWaitlistPayload(form: HTMLFormElement): Record<string, string | null> {
  const formData = new FormData(form);
  const url = new URL(window.location.href);
  return {
    company: formDataString(formData, 'company'),
    email: formDataString(formData, 'email') ?? '',
    message: formDataString(formData, 'message'),
    page_url: window.location.href,
    referrer: document.referrer || null,
    source_path: window.location.pathname,
    utm_campaign: url.searchParams.get('utm_campaign'),
    utm_content: url.searchParams.get('utm_content'),
    utm_medium: url.searchParams.get('utm_medium'),
    utm_source: url.searchParams.get('utm_source'),
    utm_term: url.searchParams.get('utm_term'),
  };
}

export async function submitCloudWaitlistForm(
  form: HTMLFormElement,
  fetchImpl: typeof fetch = fetch,
): Promise<void> {
  const submitButton = form.querySelector<HTMLButtonElement>('button[type="submit"]');
  const endpoint = form.action || CLOUD_WAITLIST_ENDPOINT;
  submitButton?.setAttribute('disabled', 'true');
  setCloudWaitlistStatus(form, 'working', 'Submitting...');
  try {
    const response = await fetchImpl(endpoint, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(buildCloudWaitlistPayload(form)),
    });
    if (!response.ok) {
      setCloudWaitlistStatus(
        form,
        'error',
        response.status === 429
          ? 'Too many attempts. Please try again later.'
          : 'Something went wrong. Please email support@ctx.rs.',
      );
      return;
    }
    form.reset();
    setCloudWaitlistStatus(form, 'success', "Thanks - we'll be in touch.");
  } catch {
    setCloudWaitlistStatus(form, 'error', 'Something went wrong. Please email support@ctx.rs.');
  } finally {
    submitButton?.removeAttribute('disabled');
  }
}

export function handleCloudWaitlistSubmit(
  event: SubmitEvent,
  fetchImpl: typeof fetch = fetch,
): Promise<void> | null {
  const target = event.target;
  if (!(target instanceof HTMLFormElement) || !target.matches('[data-cloud-waitlist-form]')) {
    return null;
  }
  event.preventDefault();
  return submitCloudWaitlistForm(target, fetchImpl);
}

function sameTabNeighbors(
  page: PageRecord,
  neighbors: PageWithNeighbors | null,
): PageWithNeighbors | null {
  if (!neighbors) {
    return null;
  }

  const previous = neighbors.previous?.tabKey === page.tabKey ? neighbors.previous : null;
  const next = neighbors.next?.tabKey === page.tabKey ? neighbors.next : null;

  if (!previous && !next) {
    return null;
  }

  return { next, previous };
}

interface SearchDialogFallbackProps {
  errorMessage: string | null;
  onClose: () => void;
  onRetry: () => void;
}

function SearchDialogFallback({
  errorMessage,
  onClose,
  onRetry,
}: SearchDialogFallbackProps) {
  return (
    <div
      role="presentation"
      onClick={onClose}
      style={{
        position: 'fixed',
        inset: 0,
        zIndex: 60,
        display: 'grid',
        placeItems: 'start center',
        padding: '3rem 1rem 1rem',
        background: 'rgba(2, 6, 23, 0.2)',
        backdropFilter: 'blur(8px)',
      }}
    >
      <div
        aria-label="Search docs"
        aria-modal="true"
        onClick={(event) => event.stopPropagation()}
        role="dialog"
        style={{
          width: 'min(40rem, calc(100vw - 2rem))',
          border: '1px solid rgba(238, 241, 239, 0.08)',
          borderRadius: '1rem',
          background: 'rgba(11, 14, 12, 0.96)',
          boxShadow: '0 32px 120px rgba(0, 0, 0, 0.46)',
          overflow: 'hidden',
        }}
      >
        <div
          style={{
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'space-between',
            gap: '1rem',
            padding: '0.95rem 1rem',
            color: '#d6d9d7',
          }}
        >
          <span>{errorMessage ? 'Search failed to load.' : 'Loading search…'}</span>
          <div style={{ display: 'flex', gap: '0.5rem' }}>
            {errorMessage ? (
              <button
                onClick={onRetry}
                style={{
                  padding: '0.4rem 0.7rem',
                  borderRadius: '0.5rem',
                  background: 'rgba(238, 241, 239, 0.08)',
                  color: '#f3f6f4',
                  cursor: 'pointer',
                }}
                type="button"
              >
                Retry
              </button>
            ) : null}
            <button
              onClick={onClose}
              style={{
                padding: '0.4rem 0.7rem',
                borderRadius: '0.5rem',
                background: 'rgba(238, 241, 239, 0.08)',
                color: '#f3f6f4',
                cursor: 'pointer',
              }}
              type="button"
            >
              Close
            </button>
          </div>
        </div>
        {errorMessage ? (
          <div
            style={{
              padding: '0 1rem 1rem',
              color: '#9fa2a0',
              fontSize: '0.84rem',
              lineHeight: 1.45,
            }}
          >
            {errorMessage}
          </div>
        ) : null}
      </div>
    </div>
  );
}

export function App({ pathname }: AppProps) {
  const site = getSiteData();
  const [currentPathname, setCurrentPathname] = useState(pathname);
  const shellRef = useRef<HTMLDivElement | null>(null);
  const initialRouteRef = useRef(true);
  const navigationModeRef = useRef<'initial' | 'pop' | 'push'>('initial');
  const resolvedPathname = getResolvedPathname(currentPathname);
  const page = getPageForPathname(resolvedPathname);
  const activeTab = getActiveTab(resolvedPathname);
  const sidebarGroups = getPageGroupsForTab(activeTab.key);
  const isStandalonePage = page?.pathname === '/teams';
  const neighbors: PageWithNeighbors | null = page
    ? sameTabNeighbors(page, getAdjacentPages(page.pathname))
    : null;
  const isHomePage = page?.pathname === '/';
  const pageGroupLabel = page?.groupLabel ?? activeTab.label;
  const [mobileNavOpen, setMobileNavOpen] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  const [SearchDialogComponent, setSearchDialogComponent] = useState<SearchDialogView | null>(
    null,
  );
  const [searchDialogStatus, setSearchDialogStatus] = useState<SearchDialogLoadStatus>('idle');
  const [searchDialogErrorMessage, setSearchDialogErrorMessage] = useState<string | null>(null);
  const searchDialogComponentRef = useRef<SearchDialogView | null>(null);
  const searchDialogStatusRef = useRef<SearchDialogLoadStatus>('idle');

  useEffect(() => {
    setCurrentPathname(pathname);
  }, [pathname]);

  useEffect(() => {
    searchDialogComponentRef.current = SearchDialogComponent;
  }, [SearchDialogComponent]);

  useEffect(() => {
    searchDialogStatusRef.current = searchDialogStatus;
  }, [searchDialogStatus]);

  function ensureSearchDialogLoaded(): void {
    if (
      searchDialogComponentRef.current ||
      searchDialogStatusRef.current === 'loading'
    ) {
      return;
    }

    searchDialogStatusRef.current = 'loading';
    setSearchDialogStatus('loading');
    setSearchDialogErrorMessage(null);

    void loadSearchDialogComponent()
      .then((nextComponent) => {
        searchDialogComponentRef.current = nextComponent;
        searchDialogStatusRef.current = 'ready';
        setSearchDialogComponent(() => nextComponent);
        setSearchDialogStatus('ready');
      })
      .catch((error: unknown) => {
        searchDialogStatusRef.current = 'error';
        setSearchDialogStatus('error');
        setSearchDialogErrorMessage(
          error instanceof Error ? error.message : 'Search failed to load.',
        );
      });
  }

  function handleOpenSearch(): void {
    ensureSearchDialogLoaded();
    setSearchOpen(true);
  }

  function handleSearchIntent(): void {
    ensureSearchDialogLoaded();
  }

  useEffect(() => {
    if (typeof document === 'undefined' || typeof window === 'undefined') {
      return;
    }

    const handlePopState = () => {
      navigationModeRef.current = 'pop';
      setCurrentPathname(window.location.pathname);
    };

    const handleDocumentClick = (event: MouseEvent) => {
      const target = event.target;
      if (!(target instanceof Element)) {
        return;
      }

      const anchor = target.closest('a[href]');
      if (!(anchor instanceof HTMLAnchorElement)) {
        return;
      }

      const navigationTarget = getClientNavigationTarget(
        window.location.href,
        {
          download: anchor.hasAttribute('download'),
          href: anchor.getAttribute('href'),
          target: anchor.getAttribute('target'),
        },
        {
          altKey: event.altKey,
          button: event.button,
          ctrlKey: event.ctrlKey,
          defaultPrevented: event.defaultPrevented,
          metaKey: event.metaKey,
          shiftKey: event.shiftKey,
        },
      );

      if (!navigationTarget) {
        return;
      }

      event.preventDefault();
      navigationModeRef.current = 'push';
      window.history.pushState(null, '', navigationTarget.href);
      setCurrentPathname(navigationTarget.pathname);
    };

    window.addEventListener('popstate', handlePopState);
    document.addEventListener('click', handleDocumentClick);

    return () => {
      window.removeEventListener('popstate', handlePopState);
      document.removeEventListener('click', handleDocumentClick);
    };
  }, []);

  useEffect(() => {
    if (typeof document === 'undefined') {
      return;
    }

    const handleSubmit = (event: SubmitEvent) => {
      void handleCloudWaitlistSubmit(event);
    };

    document.addEventListener('submit', handleSubmit);
    return () => document.removeEventListener('submit', handleSubmit);
  }, []);

  useEffect(() => {
    applyRouteMetadata(resolvedPathname);
  }, [resolvedPathname]);

  useEffect(() => {
    setMobileNavOpen(false);
    setSearchOpen(false);
  }, [resolvedPathname]);

  useEffect(() => {
    if (typeof window === 'undefined') {
      return;
    }

    const handleKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
        event.preventDefault();
        handleOpenSearch();
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, []);

  useEffect(() => {
    if (typeof window === 'undefined' || SearchDialogComponent || searchDialogStatus !== 'idle') {
      return;
    }

    const preloadSearchDialog = () => {
      ensureSearchDialogLoaded();
    };

    if ('requestIdleCallback' in window) {
      const idleId = window.requestIdleCallback(preloadSearchDialog, { timeout: 2000 });
      return () => window.cancelIdleCallback(idleId);
    }

    const timeoutId = window.setTimeout(preloadSearchDialog, 1200);
    return () => window.clearTimeout(timeoutId);
  }, [SearchDialogComponent, searchDialogStatus]);

  useEffect(() => {
    if (!searchOpen || SearchDialogComponent || typeof window === 'undefined') {
      return;
    }

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        setSearchOpen(false);
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [SearchDialogComponent, searchOpen]);

  useEffect(() => {
    if (typeof window === 'undefined') {
      return;
    }

    if (initialRouteRef.current) {
      initialRouteRef.current = false;
      return;
    }

    window.requestAnimationFrame(() => {
      const navigationMode = navigationModeRef.current;
      navigationModeRef.current = 'push';

      if (window.location.hash) {
        const decodedHash = decodeURIComponent(window.location.hash.slice(1));
        const target = document.getElementById(decodedHash);
        if (target) {
          target.scrollIntoView();
          return;
        }
      }

      if (navigationMode === 'push') {
        window.scrollTo({ left: 0, top: 0 });
      }
    });
  }, [resolvedPathname]);

  function navigateFromSearch(href: string): void {
    if (typeof window === 'undefined') {
      return;
    }

    const target = getSearchNavigationTarget(window.location.href, href);
    if (!target) {
      return;
    }

    setSearchOpen(false);

    if (target.sameDocument) {
      window.history.pushState(null, '', target.href);

      if (target.hash) {
        const decodedHash = decodeURIComponent(target.hash.slice(1));
        const anchorTarget = document.getElementById(decodedHash);
        if (anchorTarget) {
          anchorTarget.scrollIntoView();
          return;
        }
      }

      window.scrollTo({ left: 0, top: 0 });
      return;
    }

    navigationModeRef.current = 'push';
    window.history.pushState(null, '', target.href);
    setCurrentPathname(target.pathname);
  }

  useEffect(() => {
    if (typeof window === 'undefined') {
      return;
    }

    const shell = shellRef.current;
    if (!shell) {
      return;
    }

    const footer = shell.querySelector<HTMLElement>('.site-footer-shell');
    const sidebarPanel = shell.querySelector<HTMLElement>('.sidebar-panel');
    const tocShell = shell.querySelector<HTMLElement>('.toc-shell');
    let frameId: number | null = null;
    let sidebarScrollbarHideTimer: number | null = null;

    const hideSidebarScrollbar = () => {
      sidebarScrollbarHideTimer = null;
      sidebarPanel?.classList.remove('is-scrollbar-active');
    };

    const showSidebarScrollbar = () => {
      if (!sidebarPanel) {
        return;
      }

      sidebarPanel.classList.add('is-scrollbar-active');
      if (sidebarScrollbarHideTimer !== null) {
        window.clearTimeout(sidebarScrollbarHideTimer);
      }
      sidebarScrollbarHideTimer = window.setTimeout(hideSidebarScrollbar, 850);
    };

    const updateFooterOverlap = () => {
      frameId = null;
      if (!footer) {
        shell.style.setProperty('--footer-overlap', '0px');
        return;
      }

      const footerRect = footer.getBoundingClientRect();
      const footerOverlap = Math.max(0, window.innerHeight - footerRect.top);
      shell.style.setProperty('--footer-overlap', `${footerOverlap}px`);

      if (tocShell) {
        const nextFooterVisible = footerOverlap > 0 ? 'true' : 'false';
        if (tocShell.dataset.footerVisible !== nextFooterVisible) {
          tocShell.dataset.footerVisible = nextFooterVisible;
        }
      }

      if (sidebarPanel) {
        const maxScroll = Math.max(0, sidebarPanel.scrollHeight - sidebarPanel.clientHeight);
        const nextScrollTop = getFooterAwareSidebarScrollTop({
          footerHeight: footerRect.height,
          footerOverlap,
          maxScroll,
        });
        if (Math.abs(sidebarPanel.scrollTop - nextScrollTop) > 0.5) {
          sidebarPanel.scrollTop = nextScrollTop;
        }
      }
    };

    const scheduleFooterOverlapUpdate = () => {
      if (frameId !== null) {
        return;
      }

      frameId = window.requestAnimationFrame(updateFooterOverlap);
    };

    scheduleFooterOverlapUpdate();
    window.addEventListener('resize', scheduleFooterOverlapUpdate);
    window.addEventListener('scroll', scheduleFooterOverlapUpdate, { passive: true });
    sidebarPanel?.addEventListener('scroll', showSidebarScrollbar, { passive: true });

    return () => {
      if (frameId !== null) {
        window.cancelAnimationFrame(frameId);
      }
      if (sidebarScrollbarHideTimer !== null) {
        window.clearTimeout(sidebarScrollbarHideTimer);
      }
      sidebarPanel?.classList.remove('is-scrollbar-active');
      sidebarPanel?.removeEventListener('scroll', showSidebarScrollbar);
      window.removeEventListener('resize', scheduleFooterOverlapUpdate);
      window.removeEventListener('scroll', scheduleFooterOverlapUpdate);
    };
  }, [resolvedPathname]);

  return (
    <div
      className="site-shell"
      data-pathname={resolvedPathname}
      data-standalone={isStandalonePage ? 'true' : undefined}
      ref={shellRef}
    >
      <Header
        activeTabKey={activeTab.key}
        onOpenMenu={() => setMobileNavOpen(true)}
        onOpenSearch={handleOpenSearch}
        onSearchIntent={handleSearchIntent}
        pageGroupLabel={pageGroupLabel}
        pageTitle={page?.title}
        tabs={site.tabs}
      />
      <div className="site-layout">
        {!isStandalonePage ? (
          <Sidebar
            activePathname={page?.pathname ?? resolvedPathname}
            groups={sidebarGroups}
            isOpen={mobileNavOpen}
            onClose={() => setMobileNavOpen(false)}
          />
        ) : null}
        <main className="content-shell">
          <div className="content-frame">
            {page ? (
              <article
                className="article-shell"
                data-pathname={page.pathname}
                data-standalone={isStandalonePage ? 'true' : undefined}
                data-tab={page.tabKey}
              >
                {!isHomePage && !isStandalonePage ? (
                  <header className="article-header">
                    <div className="article-section-label">{pageGroupLabel}</div>
                    <div className="article-title-row">
                      <h1>{page.title}</h1>
                      <PageActions
                        className="page-actions-desktop"
                        onCopy={() => copyPageUrl(page.pathname)}
                      />
                    </div>
                    {page.descriptionHtml ? (
                      <div
                        className="article-description"
                        dangerouslySetInnerHTML={{ __html: page.descriptionHtml }}
                      />
                    ) : null}
                    <PageActions
                      className="page-actions-mobile"
                      onCopy={() => copyPageUrl(page.pathname)}
                    />
                  </header>
                ) : null}
                <PageContent html={page.html} pathname={page.pathname} />
                {!isStandalonePage ? <PageFooterNav neighbors={neighbors} /> : null}
              </article>
            ) : (
              <article className="article-shell">
                <header className="article-header">
                  <div className="article-section-label">Docs</div>
                  <h1>Page not found</h1>
                  <p className="missing-page-copy">
                    This route is not part of the current docs set.
                  </p>
                </header>
              </article>
            )}
          </div>
        </main>
        {!isStandalonePage ? <TableOfContents headings={page?.headings ?? []} /> : null}
        <SiteFooter footer={site.footer} />
      </div>
      {SearchDialogComponent ? (
        <SearchDialogComponent
          isOpen={searchOpen}
          onClose={() => setSearchOpen(false)}
          onNavigate={navigateFromSearch}
        />
      ) : searchOpen ? (
        <SearchDialogFallback
          errorMessage={searchDialogStatus === 'error' ? searchDialogErrorMessage : null}
          onClose={() => setSearchOpen(false)}
          onRetry={ensureSearchDialogLoaded}
        />
      ) : null}
    </div>
  );
}
