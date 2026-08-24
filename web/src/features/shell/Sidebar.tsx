import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useRouter } from "@tanstack/react-router";

import { useAuth } from "@/features/auth/auth-context";
import { statsQueryOptions } from "@/features/stats/queries";
import { SECTIONS, type SectionId } from "@/lib/constants";

const SECTION_TO: Record<SectionId, `/${SectionId}`> = {
  dashboard: "/dashboard",
  stats: "/stats",
  settings: "/settings",
  tokens: "/tokens",
  keys: "/keys",
  nodes: "/nodes",
  logs: "/logs",
  playground: "/playground",
};

/**
 * Graphite rail: wordmark, one Link per SECTIONS entry with router active state,
 * and the session foot (expiry chip + explicit logout).
 */
export function Sidebar() {
  const auth = useAuth();
  const qc = useQueryClient();
  const router = useRouter();
  // Optional chip — soft-fail; never block the rail.
  const statsQ = useQuery({ ...statsQueryOptions, retry: false });
  const schemaVersion = statsQ.data?.schemaVersion;

  function handleLogout() {
    // Explicit logout recipe: do NOT call endAdminSession (would double-clear).
    auth.logout();
    qc.clear();
    void router.navigate({ to: "/login", search: { redirect: undefined } });
    void router.invalidate();
  }

  return (
    <div className="rail">
      <div className="rail__brand">
        <Link to="/dashboard" className="wordmark">
          Serpotter<span className="wordmark__dot">.</span>
        </Link>
      </div>

      <nav className="rail__nav" aria-label="Admin sections">
        <ul className="rail__list">
          {SECTIONS.map((s) => (
            <li key={s.id}>
              <Link
                to={SECTION_TO[s.id]}
                className="rail__link"
                activeProps={{ className: "rail__link is-active" }}
              >
                <span>{s.label}</span>
                <span className="rail__hint">#{s.id}</span>
              </Link>
            </li>
          ))}
        </ul>
      </nav>

      <div className="rail__foot">
        {schemaVersion != null || auth.sessionExpiresAt ? (
          <div className="rail__meta">
            {schemaVersion != null ? (
              <span className="chip chip--live">
                <span className="chip__swatch" aria-hidden />
                schema {schemaVersion}
              </span>
            ) : null}
            {auth.sessionExpiresAt ? (
              <span className="chip" title={`Admin session expires ${auth.sessionExpiresAt}`}>
                <span className="chip__swatch" aria-hidden />
                exp {auth.sessionExpiresAt}
              </span>
            ) : null}
          </div>
        ) : null}
        <button type="button" className="btn btn--ghost btn--sm" onClick={handleLogout}>
          Log out
        </button>
      </div>
    </div>
  );
}
