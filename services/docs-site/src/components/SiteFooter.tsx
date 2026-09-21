import React from 'react';
import { toSiteHref } from '../lib/route-hrefs';
import type { SiteFooterConfig } from '../types';
import { GitHubIcon, SlackIcon, XIcon } from './Icons';

void React;

interface SiteFooterProps {
  footer: SiteFooterConfig | null;
}

function isExternalHref(href: string): boolean {
  return href.startsWith('http://') || href.startsWith('https://');
}

export function SiteFooter({ footer }: SiteFooterProps) {
  if (!footer) {
    return null;
  }

  const currentYear = new Date().getFullYear();

  return (
    <footer className="site-footer-shell">
      <div className="site-footer-inner">
        <div className="site-footer-main">
          <div className="site-footer-brand-column">
            <a aria-label="ctx home page" className="site-footer-brand" href="/">
              <span className="brand-wordmark site-footer-wordmark">
                <span className="brand-wordmark-text">ctx</span>
                <span aria-hidden="true" className="brand-wordmark-cursor" />
              </span>
            </a>
            <div className="site-footer-brand-meta">
              <div className="site-footer-socials">
                {footer.socials.github ? (
                  <a
                    aria-label="GitHub"
                    className="site-footer-social-link"
                    href={footer.socials.github}
                    rel="noreferrer"
                    target="_blank"
                  >
                    <GitHubIcon className="site-footer-social-icon" />
                  </a>
                ) : null}
                {footer.socials.x ? (
                  <a
                    aria-label="X"
                    className="site-footer-social-link"
                    href={footer.socials.x}
                    rel="noreferrer"
                    target="_blank"
                  >
                    <XIcon className="site-footer-social-icon" />
                  </a>
                ) : null}
                {footer.socials.slack ? (
                  <a
                    aria-label="Slack"
                    className="site-footer-social-link"
                    href={footer.socials.slack}
                    rel="noreferrer"
                    target="_blank"
                  >
                    <SlackIcon className="site-footer-social-icon" />
                  </a>
                ) : null}
              </div>
              <p className="site-footer-legal-text">
                &copy; {currentYear} ctx engineering inc.
              </p>
            </div>
          </div>
          <div className="site-footer-columns">
            {footer.links.map((group) => (
              <section className="site-footer-column" key={group.header}>
                <h2 className="site-footer-column-title">{group.header}</h2>
                <div className="site-footer-column-links">
                  {group.items.map((item) => (
                    <a
                      className="site-footer-column-link"
                      href={toSiteHref(item.href)}
                      key={`${group.header}:${item.label}`}
                      rel={isExternalHref(item.href) ? 'noreferrer' : undefined}
                      target={isExternalHref(item.href) ? '_blank' : undefined}
                    >
                      {item.label}
                    </a>
                  ))}
                </div>
              </section>
            ))}
          </div>
        </div>
      </div>
    </footer>
  );
}
