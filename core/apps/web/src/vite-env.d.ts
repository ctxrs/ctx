/// <reference types="vite/client" />
/// <reference types="vitest/globals" />

interface ImportMetaEnv {
  readonly VITE_CTX_WAL_MODE?: string;
  readonly VITE_CTX_WAL_ENDPOINT?: string;
  readonly VITE_POSTHOG_KEY?: string;
  readonly VITE_POSTHOG_ENV?: "staging" | "production";
  readonly VITE_POSTHOG_HOST?: string;
  readonly VITE_POSTHOG_PROJECT_ID?: string;
  readonly VITE_POSTHOG_UI_HOST?: string;
  readonly VITE_POSTHOG_CAPTURE_IN_DEV?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

declare module "*.png" {
  const src: string;
  export default src;
}

declare module "*.jpg" {
  const src: string;
  export default src;
}

declare module "*.jpeg" {
  const src: string;
  export default src;
}

declare module "*.svg" {
  const src: string;
  export default src;
}

declare module "*.gif" {
  const src: string;
  export default src;
}

declare module "*.webp" {
  const src: string;
  export default src;
}

declare const __CTX_APP_VERSION__: string;
declare const __CTX_BUILD_CI__: boolean;
