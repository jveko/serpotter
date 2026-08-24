import { Link } from "@tanstack/react-router";

import { relativeTime } from "@/lib/relative-time";
import type { RequestLogRow } from "@/features/logs/types";

export function RecentActivity({ rows }: { rows: RequestLogRow[] }) {
  if (rows.length === 0) return <p className="empty">No recent requests.</p>;
  return (
    <ul className="activity-list">
      {rows.map((r) => (
        <li key={r.id}>
          <Link
            to="/logs"
            search={r.requestId ? { requestId: r.requestId } : {}}
            className={`activity-row status-${r.status >= 500 ? "bad" : r.status >= 400 ? "warn" : "ok"}`}
          >
            <span className="dot" aria-hidden />
            <span className="activity-path">{r.path}</span>
            <span className="activity-svc">{r.service ?? "—"}</span>
            <span className="num">{r.durationMs != null ? `${r.durationMs}ms` : "—"}</span>
            <time dateTime={r.createdAt}>{relativeTime(r.createdAt)}</time>
          </Link>
        </li>
      ))}
    </ul>
  );
}
