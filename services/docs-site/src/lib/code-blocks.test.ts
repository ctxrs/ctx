import { describe, expect, it } from 'vitest';
import { transformCodeBlocksHtml } from './code-blocks';

describe('transformCodeBlocksHtml', () => {
  it('wraps fenced code blocks with copy controls and shiki markup', async () => {
    const html = '<pre><code class="language-bash">curl -fsSL https://ctx.rs/install | sh\n</code></pre>';
    const result = await transformCodeBlocksHtml(html);

    expect(result).toContain('class="ctx-code-block"');
    expect(result).toContain('data-copy-code');
    expect(result).toContain('aria-label="Copy the contents from the code block"');
    expect(result).toContain('class="ctx-code-block-pre shiki shiki-themes');
    expect(result).toContain('github-light-default');
    expect(result).toContain('dark-plus');
    expect(result).toContain('--shiki-dark:#DCDCAA');
    expect(result).toContain('--shiki-dark:#569CD6');
    expect(result).toContain('ctx-code-copy-icon-pair-check');
  });

  it('keeps a dark background override and preserves JSON token colors', async () => {
    const html = '<pre><code class="language-json">{&quot;ok&quot;: true, &quot;count&quot;: 2}</code></pre>';
    const result = await transformCodeBlocksHtml(html);

    expect(result).toContain('--shiki-dark-bg:#0B0C0E');
    expect(result).toContain('--shiki-dark:#9CDCFE');
    expect(result).toContain('--shiki-dark:#569CD6');
    expect(result).toContain('--shiki-dark:#B5CEA8');
  });
});
