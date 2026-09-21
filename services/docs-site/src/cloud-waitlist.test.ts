import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  handleCloudWaitlistSubmit,
  submitCloudWaitlistForm,
} from './App';
import { renderRoute } from './render';

const PRODUCTION_WAITLIST_ENDPOINT = 'https://api.ctx.rs/functions/v1/cloud-waitlist';

class FixtureHtmlFormElement {
  readonly action = PRODUCTION_WAITLIST_ENDPOINT;
  readonly button = {
    removeAttribute: vi.fn(),
    setAttribute: vi.fn(),
  };
  readonly matches = vi.fn(
    (selector: string) => selector === '[data-cloud-waitlist-form]',
  );
  readonly reset = vi.fn();
  readonly status = {
    dataset: {} as Record<string, string>,
    textContent: '',
  };

  constructor(readonly values: Record<string, string>) {}

  querySelector(selector: string): unknown {
    return selector === 'button[type="submit"]' ? this.button : this.status;
  }
}

class FixtureFormData {
  readonly fixture: FixtureHtmlFormElement;

  constructor(form: HTMLFormElement) {
    this.fixture = form as unknown as FixtureHtmlFormElement;
  }

  get(key: string): string | null {
    return this.fixture.values[key] ?? null;
  }
}

function installBrowserContext(): void {
  vi.stubGlobal('FormData', FixtureFormData);
  vi.stubGlobal('HTMLFormElement', FixtureHtmlFormElement);
  vi.stubGlobal('document', {
    referrer: 'https://github.com/ctxrs/ctx',
  });
  vi.stubGlobal('window', {
    location: {
      href: 'https://ctx.rs/teams/?utm_campaign=launch&utm_medium=email&utm_source=newsletter',
      pathname: '/teams/',
    },
  });
}

function waitlistForm(values: Record<string, string> = {}): FixtureHtmlFormElement {
  return new FixtureHtmlFormElement(values);
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('cloud waitlist contract', () => {
  it('renders the retained fields, status target, and POST event markers', () => {
    const rendered = renderRoute('/teams/');
    const formHtml = rendered.html.match(
      /<form\b[^>]*data-cloud-waitlist-form[^>]*>[\s\S]*?<\/form>/u,
    )?.[0];

    expect(formHtml).toContain(`action="${PRODUCTION_WAITLIST_ENDPOINT}"`);
    expect(formHtml).toContain('method="post"');
    expect(formHtml).toContain('data-cloud-waitlist-form');
    expect(formHtml).toMatch(/<input\b[^>]*name="company"[^>]*>/u);
    expect(formHtml).toMatch(/<input\b[^>]*name="email"[^>]*>/u);
    expect(formHtml).toMatch(/<textarea\b[^>]*name="message"[^>]*>/u);
    expect(formHtml).toMatch(
      /<input\b(?=[^>]*name="source")(?=[^>]*type="hidden")[^>]*>/u,
    );
    expect(formHtml).toMatch(/<button\b[^>]*type="submit"[^>]*>/u);
    expect(formHtml).toMatch(/<p\b[^>]*data-cloud-waitlist-status[^>]*>/u);
  });

  it('wires submit events to the bounded JSON request and success state', async () => {
    installBrowserContext();
    const fixture = waitlistForm({
      company: '   ',
      email: '  engineer@example.com ',
      message: '  Shared team history  ',
    });
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(new Response(null, {
      status: 204,
    }));
    const preventDefault = vi.fn();
    const event = {
      preventDefault,
      target: fixture,
    } as unknown as SubmitEvent;

    const submission = handleCloudWaitlistSubmit(event, fetchMock);
    expect(submission).not.toBeNull();
    await submission;

    expect(preventDefault).toHaveBeenCalledOnce();
    expect(fixture.matches).toHaveBeenCalledWith('[data-cloud-waitlist-form]');
    expect(fetchMock).toHaveBeenCalledOnce();
    expect(fetchMock).toHaveBeenCalledWith(PRODUCTION_WAITLIST_ENDPOINT, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        company: null,
        email: 'engineer@example.com',
        message: 'Shared team history',
        page_url: 'https://ctx.rs/teams/?utm_campaign=launch&utm_medium=email&utm_source=newsletter',
        referrer: 'https://github.com/ctxrs/ctx',
        source_path: '/teams/',
        utm_campaign: 'launch',
        utm_content: null,
        utm_medium: 'email',
        utm_source: 'newsletter',
        utm_term: null,
      }),
    });
    expect(fixture.reset).toHaveBeenCalledOnce();
    expect(fixture.status).toEqual({
      dataset: { tone: 'success' },
      textContent: "Thanks - we'll be in touch.",
    });
    expect(fixture.button.setAttribute).toHaveBeenCalledWith('disabled', 'true');
    expect(fixture.button.removeAttribute).toHaveBeenCalledWith('disabled');
  });

  it('preserves the generic HTTP failure state without resetting the form', async () => {
    installBrowserContext();
    const fixture = waitlistForm({ email: 'engineer@example.com' });
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(new Response(null, {
      status: 500,
    }));

    await submitCloudWaitlistForm(fixture as unknown as HTMLFormElement, fetchMock);

    expect(fixture.reset).not.toHaveBeenCalled();
    expect(fixture.status).toEqual({
      dataset: { tone: 'error' },
      textContent: 'Something went wrong. Please email support@ctx.rs.',
    });
    expect(fixture.button.setAttribute).toHaveBeenCalledWith('disabled', 'true');
    expect(fixture.button.removeAttribute).toHaveBeenCalledWith('disabled');
  });

  it('preserves rate-limit and network error states without resetting the form', async () => {
    installBrowserContext();
    const rateLimited = waitlistForm({ email: 'engineer@example.com' });
    const rateLimitFetch = vi.fn<typeof fetch>().mockResolvedValue(new Response(null, {
      status: 429,
    }));

    await submitCloudWaitlistForm(
      rateLimited as unknown as HTMLFormElement,
      rateLimitFetch,
    );

    expect(rateLimited.reset).not.toHaveBeenCalled();
    expect(rateLimited.status).toEqual({
      dataset: { tone: 'error' },
      textContent: 'Too many attempts. Please try again later.',
    });
    expect(rateLimited.button.removeAttribute).toHaveBeenCalledWith('disabled');

    const unavailable = waitlistForm({ email: 'engineer@example.com' });
    const unavailableFetch = vi.fn<typeof fetch>().mockRejectedValue(new Error('offline'));

    await submitCloudWaitlistForm(
      unavailable as unknown as HTMLFormElement,
      unavailableFetch,
    );

    expect(unavailable.reset).not.toHaveBeenCalled();
    expect(unavailable.status).toEqual({
      dataset: { tone: 'error' },
      textContent: 'Something went wrong. Please email support@ctx.rs.',
    });
    expect(unavailable.button.removeAttribute).toHaveBeenCalledWith('disabled');
  });
});
