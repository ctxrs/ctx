import React from 'react';
import type { PageGroup } from '../types';

void React;

interface SidebarProps {
  activePathname: string;
  groups: PageGroup[];
  isOpen: boolean;
  onClose: () => void;
}

export function Sidebar({
  activePathname,
  groups,
  isOpen,
  onClose,
}: SidebarProps) {
  return (
    <>
      {isOpen ? (
        <button
          aria-label="Close navigation"
          className="mobile-sidebar-backdrop"
          onClick={onClose}
          type="button"
        />
      ) : null}
      <aside className={`sidebar-shell${isOpen ? ' is-open' : ''}`}>
        <div className="sidebar-panel">
          {groups.map((group) => (
            <section className="sidebar-group" key={group.label}>
              <div className="sidebar-group-title">{group.label}</div>
              <ul className="sidebar-group-list">
                {group.pages.map((page) => (
                  <li key={page.pathname}>
                    <a
                      className={`sidebar-link${page.pathname === activePathname ? ' is-active' : ''}`}
                      href={page.href}
                    >
                      {page.sidebarTitle}
                    </a>
                  </li>
                ))}
              </ul>
            </section>
          ))}
        </div>
      </aside>
    </>
  );
}
