import { getSingletonHighlighter } from 'shiki';

const COPY_ICON_SVG =
  '<svg viewBox="0 0 18 18" aria-hidden="true" class="ctx-code-copy-icon"><path d="M14.25 5.25H7.25C6.14543 5.25 5.25 6.14543 5.25 7.25V14.25C5.25 15.3546 6.14543 16.25 7.25 16.25H14.25C15.3546 16.25 16.25 15.3546 16.25 14.25V7.25C16.25 6.14543 15.3546 5.25 14.25 5.25Z" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"></path><path d="M2.80103 11.998L1.77203 5.07397C1.61003 3.98097 2.36403 2.96397 3.45603 2.80197L10.38 1.77297C11.313 1.63397 12.19 2.16297 12.528 3.00097" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"></path></svg>';
const CHECK_ICON_SVG =
  '<svg viewBox="0 0 16 16" aria-hidden="true" class="ctx-code-check-icon"><path d="m3.75 8.5 2.25 2.25 6-6" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"></path></svg>';

const SHIKI_THEME_LIGHT = 'github-light-default';
const SHIKI_THEME_DARK = 'dark-plus';
const SHIKI_DARK_BACKGROUND = '#0B0C0E';

const LANGUAGE_ALIASES: Record<string, string> = {
  bash: 'bash',
  curl: 'bash',
  html: 'html',
  java: 'java',
  javascript: 'javascript',
  js: 'javascript',
  json: 'json',
  md: 'markdown',
  markdown: 'markdown',
  mdx: 'mdx',
  plain: 'text',
  python: 'python',
  shell: 'bash',
  shellscript: 'bash',
  sh: 'bash',
  text: 'text',
  ts: 'typescript',
  tsx: 'tsx',
  typescript: 'typescript',
  yaml: 'yaml',
  yml: 'yaml',
  zsh: 'bash',
};

const highlighterPromise = getSingletonHighlighter({
  langs: Array.from(new Set(Object.values(LANGUAGE_ALIASES))),
  themes: [SHIKI_THEME_LIGHT, SHIKI_THEME_DARK],
});

function escapeHtml(value: string): string {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

function decodeHtml(value: string): string {
  return value
    .replaceAll('&quot;', '"')
    .replaceAll('&#39;', "'")
    .replaceAll('&lt;', '<')
    .replaceAll('&gt;', '>')
    .replaceAll('&amp;', '&');
}

function normalizeLanguage(language: string | undefined): string {
  const value = (language ?? '').toLowerCase();
  return LANGUAGE_ALIASES[value] ?? 'text';
}

function decorateHighlightedHtml(highlightedHtml: string, language: string): string {
  return highlightedHtml
    .replace(
      '<pre class="',
      `<pre class="ctx-code-block-pre `,
    )
    .replace(
      '<code>',
      `<code class="ctx-code-block-code" language="${escapeHtml(language)}">`,
    )
    .replace(/--shiki-dark-bg:[^;"]+/g, `--shiki-dark-bg:${SHIKI_DARK_BACKGROUND}`);
}

async function renderHighlightedCode(code: string, language: string): Promise<string> {
  const highlighter = await highlighterPromise;
  const highlightedHtml = highlighter.codeToHtml(code, {
    defaultColor: false,
    lang: language,
    themes: {
      dark: SHIKI_THEME_DARK,
      light: SHIKI_THEME_LIGHT,
    },
  });

  return decorateHighlightedHtml(highlightedHtml, language);
}

export async function transformCodeBlocksHtml(html: string): Promise<string> {
  const blocks = Array.from(
    html.matchAll(/<pre(?:[^>]*)><code([^>]*)>([\s\S]*?)<\/code><\/pre>/g),
  );

  if (blocks.length === 0) {
    return html;
  }

  let result = html;

  for (const [blockHtml, codeAttributes, encodedCode] of blocks) {
    const languageMatch =
      blockHtml.match(/data-language="([^"]+)"/i) ??
      codeAttributes.match(/language-([a-z0-9-]+)/i) ??
      codeAttributes.match(/language="([^"]+)"/i);
    const language = normalizeLanguage(languageMatch?.[1]);
    const rawCode = decodeHtml(encodedCode).replace(/\n$/, '');
    const highlightedHtml = await renderHighlightedCode(rawCode, language);
    const replacement = `<div class="ctx-code-block" data-code="${escapeHtml(encodeURIComponent(rawCode))}" data-language="${escapeHtml(language)}"><div class="ctx-code-block-actions"><button class="ctx-code-copy-button" type="button" data-copy-code aria-label="Copy the contents from the code block" data-tooltip="Copy"><span class="ctx-code-copy-icon-pair ctx-code-copy-icon-pair-copy">${COPY_ICON_SVG}</span><span class="ctx-code-copy-icon-pair ctx-code-copy-icon-pair-check">${CHECK_ICON_SVG}</span></button></div>${highlightedHtml}</div>`;

    result = result.replace(blockHtml, replacement);
  }

  return result;
}
