import React, { useEffect, useRef, useState } from 'react';
import { copyText } from '../lib/clipboard';
import type { NavTab } from '../types';
import { ChevronIcon, EllipsisVerticalIcon, MenuIcon, SearchIcon } from './Icons';

void React;

const UNIX_INSTALL_COMMAND = 'curl -fsSL https://ctx.rs/install | sh';
const WINDOWS_INSTALL_COMMAND = 'irm https://ctx.rs/install.ps1 | iex';
const CONFIGURED_INSTALL_COMMAND =
  import.meta.env?.VITE_CTX_INSTALL_COMMAND ??
  (typeof process === 'undefined' ? undefined : process.env.VITE_CTX_INSTALL_COMMAND);
const DEFAULT_INSTALL_COMMAND = CONFIGURED_INSTALL_COMMAND ?? UNIX_INSTALL_COMMAND;

interface HeaderProps {
  activeTabKey: string;
  onOpenMenu: () => void;
  onOpenSearch: () => void;
  onSearchIntent: () => void;
  pageGroupLabel?: string;
  pageTitle?: string;
  tabs: NavTab[];
}

function isActiveLink(activeTabKey: string, tab: NavTab): boolean {
  return !tab.external && tab.key === activeTabKey;
}

function getBrowserPlatform(): string | undefined {
  if (typeof navigator === 'undefined') {
    return undefined;
  }

  return (
    (navigator as Navigator & { userAgentData?: { platform?: string } }).userAgentData?.platform ??
    navigator.platform ??
    navigator.userAgent
  );
}

export function getInstallCommandForPlatform(
  platform: string | undefined,
  configuredCommand?: string,
): string {
  if (configuredCommand !== undefined) {
    return configuredCommand;
  }
  return platform && /\bwin/i.test(platform) ? WINDOWS_INSTALL_COMMAND : UNIX_INSTALL_COMMAND;
}

function getBrowserInstallCommand(): string {
  return getInstallCommandForPlatform(getBrowserPlatform(), CONFIGURED_INSTALL_COMMAND);
}

export function formatInstallCommandDisplay(command: string): string {
  const prompt = command === WINDOWS_INSTALL_COMMAND ? 'PS> ' : '$ ';
  return `${prompt}${command.replace(/https:\/\//g, '')}`;
}

export function Header({
  activeTabKey,
  onOpenMenu,
  onOpenSearch,
  onSearchIntent,
  pageGroupLabel,
  pageTitle,
  tabs,
}: HeaderProps) {
  const [installCopied, setInstallCopied] = useState(false);
  const [installCommand, setInstallCommand] = useState(DEFAULT_INSTALL_COMMAND);
  const installResetTimerRef = useRef<number | null>(null);
  const showMobileGroup = activeTabKey !== 'blog' && pageGroupLabel && pageGroupLabel !== pageTitle;
  const installCommandDisplay = formatInstallCommandDisplay(installCommand);

  useEffect(() => {
    return () => {
      if (installResetTimerRef.current !== null) {
        window.clearTimeout(installResetTimerRef.current);
      }
    };
  }, []);

  useEffect(() => {
    setInstallCommand(getBrowserInstallCommand());
  }, []);

  async function handleCopyInstallCommand(): Promise<void> {
    try {
      await copyText(installCommand);
      setInstallCopied(true);
      if (installResetTimerRef.current !== null) {
        window.clearTimeout(installResetTimerRef.current);
      }
      installResetTimerRef.current = window.setTimeout(() => {
        setInstallCopied(false);
        installResetTimerRef.current = null;
      }, 1400);
    } catch {
      setInstallCopied(false);
    }
  }

  return (
    <header className="site-header">
      <div className="site-header-inner">
        <div className="header-top">
          <a aria-label="ctx home page" className="brand-link" href="/">
            <span className="brand-wordmark">
              <span className="brand-wordmark-text">ctx</span>
              <span aria-hidden="true" className="brand-wordmark-cursor" />
            </span>
          </a>
          <button
            aria-label="Open search"
            className="header-action-button search-button"
            onFocus={onSearchIntent}
            onMouseEnter={onSearchIntent}
            onClick={onOpenSearch}
            onPointerDown={onSearchIntent}
            type="button"
          >
            <SearchIcon className="search-button-icon" />
            <span className="search-button-label">Search...</span>
            <kbd>
              <span className="search-button-kbd-symbol">⌘</span>
              <span className="search-button-kbd-letter">K</span>
            </kbd>
          </button>
          <button
            aria-label="Copy install command"
            className={`header-install-copy${installCopied ? ' is-copied' : ''}`}
            onClick={() => {
              void handleCopyInstallCommand();
            }}
            title="Copy install command"
            type="button"
          >
            <svg
              aria-hidden="true"
              className="header-install-frame"
              preserveAspectRatio="none"
              viewBox="0 0 100 100"
            >
              <defs>
                <pattern height="6" id="cta-scan-pattern" patternUnits="userSpaceOnUse" width="2">
                  <rect fill="#33ff33" fillOpacity="0.96" height="3" width="2" x="0" y="0" />
                  <rect fill="#33ff33" fillOpacity="0.32" height="3" width="2" x="0" y="3" />
                </pattern>
                <filter height="116%" id="cta-glow-filter" width="116%" x="-8%" y="-8%">
                  <feGaussianBlur in="SourceGraphic" result="cta_frame_blur_near" stdDeviation="0.65" />
                  <feGaussianBlur in="SourceGraphic" result="cta_frame_blur_far" stdDeviation="1.2" />
                  <feMerge>
                    <feMergeNode in="cta_frame_blur_far" />
                    <feMergeNode in="cta_frame_blur_near" />
                    <feMergeNode in="SourceGraphic" />
                  </feMerge>
                </filter>
              </defs>
              <rect className="header-install-frame-glow" height="98" width="98" x="1" y="1" />
              <rect className="header-install-frame-core" height="98" width="98" x="1" y="1" />
              <rect className="header-install-frame-scan" height="98" width="98" x="1" y="1" />
            </svg>
            <span className="header-install-command">{installCommandDisplay}</span>
            <span aria-hidden="true" className="header-install-icon-wrap">
              <span className="header-install-icon-pair header-install-icon-pair-copy">
                <img alt="" className="header-install-icon" src="/icons/copy.svg" />
              </span>
              <span className="header-install-icon-pair header-install-icon-pair-check">
                <img alt="" className="header-install-icon" src="/icons/check.svg" />
              </span>
            </span>
          </button>
          <div className="mobile-header-actions">
            <button
              aria-label="Open search"
              className="mobile-icon-button"
              onFocus={onSearchIntent}
              onClick={onOpenSearch}
              onPointerDown={onSearchIntent}
              type="button"
            >
              <SearchIcon className="mobile-header-icon" />
            </button>
            <button
              aria-label="More actions"
              className="mobile-icon-button is-narrow"
              type="button"
            >
              <EllipsisVerticalIcon className="mobile-header-icon" />
            </button>
          </div>
        </div>
        <nav aria-label="Primary" className="top-nav">
          {tabs.map((tab) => (
            <a
              className={`top-nav-link${isActiveLink(activeTabKey, tab) ? ' is-active' : ''}`}
              href={tab.href}
              key={tab.key}
              rel={tab.external ? 'noreferrer' : undefined}
              target={tab.external ? '_blank' : undefined}
            >
              {tab.label}
            </a>
          ))}
        </nav>
        <button
          aria-label="Open navigation"
          className="mobile-crumb-row"
          onClick={onOpenMenu}
          type="button"
        >
          <span className="mobile-crumb-menu">
            <MenuIcon className="mobile-menu-icon" />
          </span>
          <span className="mobile-crumb-copy">
            {showMobileGroup ? (
              <span className="mobile-crumb-group">
                <span>{pageGroupLabel}</span>
                <ChevronIcon className="mobile-crumb-separator" />
              </span>
            ) : null}
            <span className="mobile-crumb-title">{pageTitle ?? 'Overview'}</span>
          </span>
        </button>
      </div>
    </header>
  );
}
