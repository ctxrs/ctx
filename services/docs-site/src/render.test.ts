import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { renderRoute } from './render';
import { getSiteData } from './lib/site-data';

describe('renderRoute', () => {
  it('renders the original search homepage without old demo media', () => {
    const home = renderRoute('/');

    expect(home.headHtml).toContain('og:title');
    expect(home.headHtml).toContain('twitter:card');
    expect(home.headHtml).toContain('"@type":"SoftwareApplication"');
    expect(home.html).not.toContain('article-header');
    expect(home.html).toContain('You have months of coding agent history on your machine');
    expect(home.html).toContain('Search it with');
    expect(home.html).not.toContain('Blame it with');
    expect(home.html).toContain('open-source CLI for fast local search across your past coding agent sessions');
    expect(home.html).toContain('Git blame for agent sessions');
    expect(home.html).not.toContain('ctx pro');
    expect(home.html).not.toContain('trial');
    expect(home.html).toContain('ctx scans it with parallel workers');
    expect(home.html).toContain('How ctx differs from agent memory and codebase intelligence');
    expect(home.html).toContain('DeepSeek Harness');
    expect(home.html).not.toContain('Agentic Development Environment');
    expect(home.html).not.toContain('<video');
  });

  it('renders Blame as an included feature with the existing examples', () => {
    const blame = renderRoute('/blame');

    expect(blame.html).toContain('git blame for agent sessions');
    expect(blame.html).toContain('included in the open-source ctx CLI');
    expect(blame.html).toContain('src/checkout.ts');
    expect(blame.html).not.toContain('$20');
    expect(blame.html).not.toContain('ctx pro');
  });

  it.each(['/pro', '/pro/', '/pro/index', '/pro/index/'])(
    'preserves %s as an alias for Blame', (pathname) => {
      expect(renderRoute(pathname)).toEqual(renderRoute('/blame'));
    },
  );

  it('preserves referral URLs as a support destination', () => {
    const legacy = renderRoute('/pro/referrals');
    expect(legacy.html).toContain('Legacy services and support');
    expect(legacy.html).toContain('mailto:support@ctx.rs');
    expect(legacy.html).not.toContain('ctx referral create');
  });

  it('exports canonical Blame discovery and preserves Markdown aliases', () => {
    const site = getSiteData();
    expect(site.redirects['/pro/index.md']).toBe('/blame.md');
    expect(site.redirects['/pro/referrals.md']).toBe('/legal/legacy-services.md');
    expect(site.pageOrder).toContain('/blame');
    expect(site.pageOrder).not.toContain('/pro');
    expect(JSON.stringify(site.tabs)).not.toContain('ctx pro');

    const asset = (name: string) => readFileSync(new URL(`../public/${name}`, import.meta.url), 'utf8');
    expect(asset('blame.md')).toContain('# Blame: git blame for agent sessions');
    expect(asset('llms.txt')).toContain('https://ctx.rs/blame.md');
    expect(asset('llms.txt')).not.toContain('https://ctx.rs/pro/');
    expect(asset('sitemap.xml')).toContain('<loc>https://ctx.rs/blame/</loc>');
    expect(asset('sitemap.xml')).not.toContain('<loc>https://ctx.rs/pro/');
    expect(readFileSync(new URL('../public/fonts/vt323-regular.woff2', import.meta.url)).length).toBeGreaterThan(0);
    expect(asset('search-index.json')).toContain('"/blame"');
  });

  it('keeps the former cloud route as an alias for the teams page', () => {
    const teams = renderRoute('/cloud');

    expect(teams.html).toContain('ctx for teams');
    expect(teams.html).toContain('Tell us about your team');
  });

  it('keeps page actions on docs pages', () => {
    const docsPage = renderRoute('/getting-started/install');

    expect(docsPage.headHtml).toContain('"@type":"BreadcrumbList"');
    expect(docsPage.html).toContain('Copy page');
  });

  it('does not duplicate a page title from a leading content heading', () => {
    const docsPage = renderRoute('/getting-started/install');

    expect(docsPage.html).toContain('<h1>Install and index local history</h1>');
    expect(docsPage.html).not.toContain('<h1 id="install-and-index-local-history">');
  });

  it('renders configured footer social links', () => {
    const home = renderRoute('/');

    expect(home.html).toContain('aria-label="GitHub"');
    expect(home.html).toContain('aria-label="X"');
    expect(home.html).toContain('aria-label="Slack"');
  });
});
