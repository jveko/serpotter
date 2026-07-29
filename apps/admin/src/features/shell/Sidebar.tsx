import { Link } from "@tanstack/react-router";

import { SECTIONS, type SectionId } from "@/lib/constants";

const SECTION_TO: Record<SectionId, `/${SectionId}`> = {
  stats: "/stats",
  settings: "/settings",
  tokens: "/tokens",
  keys: "/keys",
  nodes: "/nodes",
  logs: "/logs",
  playground: "/playground",
};

/**
 * Left nav: one Link per SECTIONS entry with router active state.
 */
export function Sidebar() {
  return (
    <nav className="sidebar" aria-label="Admin sections">
      <p className="sidebar__kicker kicker">Sections</p>
      <ul className="sidebar__list">
        {SECTIONS.map((s) => (
          <li key={s.id}>
            <Link
              to={SECTION_TO[s.id]}
              className="sidebar__link"
              activeProps={{ className: "sidebar__link is-active" }}
            >
              <span className="sidebar__label">{s.label}</span>
              <span className="sidebar__hint">#{s.id}</span>
            </Link>
          </li>
        ))}
      </ul>
    </nav>
  );
}
