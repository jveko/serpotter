import { Link } from "@tanstack/react-router";

import type { RequestLogRow } from "@/features/logs/types";

function relativeTime(iso: string): string {
  const ms = Date.now() - new Date(iso).getTime();
  const mins = Math.floor(ms / 60_000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  return `${Math.floor(hrs / 24)}d ago`;
}

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
