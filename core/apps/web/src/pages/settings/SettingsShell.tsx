import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import { TextInput } from "../../components/ui/text-input";
import type { SectionId, SettingsSectionMeta } from "./SettingsPage.types";

export function SettingsShell({
  backLink,
  query,
  onQueryChange,
  sidebarSections,
  active,
  onSectionChange,
  headerLabel,
  saveError,
  children,
}: {
  backLink: { to: string; label: string };
  query: string;
  onQueryChange: (value: string) => void;
  sidebarSections: SettingsSectionMeta[];
  active: SectionId;
  onSectionChange: (section: SectionId) => void;
  headerLabel: string;
  saveError: string | null;
  children: ReactNode;
}) {
  return (
    <div className="settings-root">
      <div className="settings-shell">
        <aside className="settings-sidebar">
          <div className="settings-sidebar-header">
            <Link className="settings-backlink" to={backLink.to}>
              {backLink.label}
            </Link>
            <div className="settings-sidebar-title">Settings</div>
          </div>

          <div className="settings-search">
            <TextInput
              className="settings-search-input"
              value={query}
              onChange={(event) => onQueryChange(event.target.value)}
              placeholder="Search settings ⌘F"
            />
          </div>

          <nav className="settings-nav" aria-label="Settings sections">
            <div className="settings-nav-group">
              {sidebarSections
                .filter((section) => section.group === "main")
                .map((section) => (
                  <button
                    key={section.id}
                    type="button"
                    className={`settings-nav-item ${active === section.id ? "settings-nav-item-active" : ""}`}
                    onClick={() => onSectionChange(section.id)}
                  >
                    {section.label}
                  </button>
                ))}
            </div>
            <div className="settings-nav-sep" aria-hidden="true" />
            <div className="settings-nav-group">
              {sidebarSections
                .filter((section) => section.group === "advanced")
                .map((section) => (
                  <button
                    key={section.id}
                    type="button"
                    className={`settings-nav-item ${active === section.id ? "settings-nav-item-active" : ""}`}
                    onClick={() => onSectionChange(section.id)}
                  >
                    {section.label}
                  </button>
                ))}
            </div>
          </nav>
        </aside>

        <main className="settings-main">
          <div className="settings-main-inner">
            <div className="settings-main-header">
              <div className="settings-main-title">{headerLabel}</div>
              {saveError ? <div className="settings-main-sub">Not saved</div> : null}
            </div>

            {saveError ? <div className="settings-banner settings-banner-error">{saveError}</div> : null}
            {children}
          </div>
        </main>
      </div>
    </div>
  );
}
