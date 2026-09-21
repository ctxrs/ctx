export async function copyText(text: string): Promise<void> {
  if (typeof window === 'undefined') {
    return;
  }

  if (window.isSecureContext && navigator.clipboard) {
    try {
      await navigator.clipboard.writeText(text);
      return;
    } catch {
      // Fall through to the DOM-based copy path when the Clipboard API is blocked.
    }
  }

  const input = document.createElement('textarea');
  input.value = text;
  input.setAttribute('readonly', '');
  input.style.position = 'fixed';
  input.style.left = '-9999px';
  document.body.append(input);
  input.select();
  document.execCommand('copy');
  input.remove();
}
