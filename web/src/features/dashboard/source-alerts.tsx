import { errorMessage, type DashboardRegion } from "./status";

/**
 * One `<p class="err" role="alert">` per failed source, with the retry control
 * outside the live region so the button is not announced as part of the
 * error. Mirrors StatsPanel's error block.
 */
export function SourceAlerts({
  regions,
  onRetry,
}: {
  regions: readonly DashboardRegion[];
  onRetry: () => void;
}) {
  const failed = regions.filter((r) => r.error != null);
  if (failed.length === 0) return null;
  return (
    <div className="block">
      {failed.map((r) => (
        <p className="err" role="alert" key={r.id}>
          {r.label}: {errorMessage(r.error) ?? "failed to load"}
        </p>
      ))}
      <div className="row">
        <button type="button" className="btn btn--secondary btn--sm" onClick={onRetry}>
          Retry
        </button>
      </div>
    </div>
  );
}
