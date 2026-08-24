import type { RequestLogRow } from "./types";

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
