export const SITE_ORIGIN =
  import.meta.env?.VITE_CTX_SITE_ORIGIN ??
  (typeof process === 'undefined' ? undefined : process.env.VITE_CTX_SITE_ORIGIN) ??
  'https://ctx.rs';
