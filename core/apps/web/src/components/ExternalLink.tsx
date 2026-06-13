import type { AnchorHTMLAttributes, MouseEvent } from "react";
import { isDesktopApp, openExternalLink } from "../utils/desktop";

export type ExternalLinkProps = Omit<AnchorHTMLAttributes<HTMLAnchorElement>, "href"> & {
  href: string;
};

export function ExternalLink({
  children,
  href,
  onClick,
  rel,
  target,
  ...rest
}: ExternalLinkProps) {
  const handleClick = (event: MouseEvent<HTMLAnchorElement>) => {
    onClick?.(event);
    if (event.defaultPrevented) return;
    if (!isDesktopApp()) return;
    if (event.button !== 0) return;
    event.preventDefault();
    void openExternalLink(href);
  };

  return (
    <a
      {...rest}
      href={href}
      onClick={handleClick}
      rel={rel ?? "noopener noreferrer"}
      target={target ?? "_blank"}
    >
      {children}
    </a>
  );
}
