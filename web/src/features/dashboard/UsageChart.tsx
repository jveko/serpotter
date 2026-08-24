import type { PerDaySeries } from "./metrics";

const W = 720;
const H = 180;
const PAD = { top: 8, right: 8, bottom: 18, left: 8 };

const FILLS = [
  "var(--accent, oklch(0.55 0.21 260))",
  "var(--ink, oklch(0.24 0.02 260))",
  "var(--graphite, oklch(0.55 0.01 260))",
  "var(--muted, oklch(0.72 0.01 260))",
];

export function UsageChart({ data, windowDays }: { data: PerDaySeries; windowDays: number }) {
  const { dates, series, errorLine } = data;
  const services = Object.keys(series);
  if (dates.length === 0) {
    return <p className="empty">No usage recorded in this window.</p>;
  }
  const maxTotal = Math.max(
    1,
    ...dates.map((_, i) => services.reduce((sum, s) => sum + series[s][i], 0)),
  );
  const innerW = W - PAD.left - PAD.right;
  const innerH = H - PAD.top - PAD.bottom;
  const slotW = innerW / dates.length;
  const barW = Math.max(2, slotW * 0.7);

  const errPoints = dates
    .map((_, i) => {
      const x = PAD.left + slotW * i + slotW / 2;
      const y = PAD.top + innerH - (errorLine[i] / maxTotal) * innerH;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");

  return (
    <figure className="usage-chart">
      <figcaption className="usage-chart__legend">
        {services.map((s, i) => (
          <span key={s} className="usage-chart__key">
            <i style={{ background: FILLS[i % FILLS.length] }} />
            {s}
          </span>
        ))}
        <span className="usage-chart__key usage-chart__key--line">
          <i className="usage-chart__errswatch" />
          errors
        </span>
        <span className="usage-chart__window">{windowDays}d window</span>
      </figcaption>
      <svg viewBox={`0 0 ${W} ${H}`} role="img" aria-label="Requests per day by service">
        {dates.map((date, i) => {
          let yCursor = PAD.top + innerH;
          const x = PAD.left + slotW * i + (slotW - barW) / 2;
          return (
            <g key={date}>
              {services.map((s, si) => {
                const h = (series[s][i] / maxTotal) * innerH;
                if (h <= 0) return null;
                yCursor -= h;
                return (
                  <rect
                    key={s}
                    x={x}
                    y={yCursor}
                    width={barW}
                    height={h}
                    fill={FILLS[si % FILLS.length]}
                  >
                    <title>{`${date} ${s}: ${series[s][i]}`}</title>
                  </rect>
                );
              })}
            </g>
          );
        })}
        <polyline points={errPoints} fill="none" stroke="var(--bad, oklch(0.55 0.22 25))" strokeWidth="1.5" />
      </svg>
    </figure>
  );
}
