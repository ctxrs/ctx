import type { ReactNode } from "react";

type MobileShellChromeProps = {
  title?: string;
  actions?: ReactNode;
  children: ReactNode;
};

export function MobileShellChrome({
  title,
  actions,
  children,
}: MobileShellChromeProps) {
  return (
    <div className="mobile-shell-page">
      <header className="mobile-shell-topbar">
        <div className="mobile-shell-brand" aria-label="ctx mobile">
          <span className="mobile-shell-brand-mark">ctx</span>
        </div>
        <div className="mobile-shell-topbar-title">{title ?? ""}</div>
        <div className="mobile-shell-topbar-actions">{actions}</div>
      </header>

      <main className="mobile-shell-content">
        {children}
      </main>
    </div>
  );
}
