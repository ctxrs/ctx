export type SessionMarkdownInlineWrapBrowserProfile = {
  id: "chromium-like" | "strict";
  allowsInlineCodeLeadingHang: boolean;
  allowsChromiumPathTailContinuation: boolean;
  allowsChromiumDottedPathBoundaryRelaxation: boolean;
};

const CHROMIUM_USER_AGENT_PATTERN = /HeadlessChrome|Chrome\/|Chromium\/|Edg\//;

export function getSessionMarkdownInlineWrapBrowserProfile(): SessionMarkdownInlineWrapBrowserProfile {
  if (typeof navigator === "undefined") {
    return {
      id: "chromium-like",
      allowsInlineCodeLeadingHang: true,
      allowsChromiumPathTailContinuation: true,
      allowsChromiumDottedPathBoundaryRelaxation: true,
    };
  }

  const chromiumLike = CHROMIUM_USER_AGENT_PATTERN.test(navigator.userAgent) || /jsdom/i.test(navigator.userAgent);
  if (chromiumLike) {
    return {
      id: "chromium-like",
      allowsInlineCodeLeadingHang: true,
      allowsChromiumPathTailContinuation: true,
      allowsChromiumDottedPathBoundaryRelaxation: true,
    };
  }

  return {
    id: "strict",
    allowsInlineCodeLeadingHang: false,
    allowsChromiumPathTailContinuation: false,
    allowsChromiumDottedPathBoundaryRelaxation: false,
  };
}

export function browserAllowsInlineCodeLeadingHang(
  profile: SessionMarkdownInlineWrapBrowserProfile = getSessionMarkdownInlineWrapBrowserProfile(),
): boolean {
  return profile.allowsInlineCodeLeadingHang;
}
