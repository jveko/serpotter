import type { RequestLogRow } from "./types";
import { formatCost, formatTokens } from "./usage";

export function RowDetail({ row }: { row: RequestLogRow }) {
  const pairs: [string, string][] = [
    ["strategy", row.strategy ?? "—"],
    ["providers consulted", row.providersConsulted ?? "—"],
    ["attempts", row.attemptCount?.toString() ?? "—"],
    ["key id", row.keyId?.toString() ?? "—"],
    ["node id", row.nodeId?.toString() ?? "—"],
    ["request id", row.requestId ?? "—"],
    ["query", row.queryPreview ?? "—"],
    ["error kind", row.errorKind ?? "—"],
    ["cost est", formatCost(row.costEst)],
    // The table already has a costEst, a token and a cacheHit column; the
    // detail states the token split as the single in/out/total line the
    // column header advertises instead of repeating three near-identical pairs.
    ["tokens (in/out/total)", formatTokens(row)],
    ["cache hit", row.cacheHit ? "yes" : "no"],
    ["provider", row.providerUsed ?? "—"],
    ["token", row.tokenName ?? "—"],
  ];
  return (
    <dl className="row-detail">
      {pairs.map(([k, v]) => (
        <div key={k} className="row-detail__pair">
          <dt>{k}</dt>
          <dd>{v}</dd>
        </div>
      ))}
    </dl>
  );
}
