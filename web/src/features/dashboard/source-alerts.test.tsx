import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { SourceAlerts } from "./source-alerts";
import { aggregateRegion, type QueryStatusLike } from "./status";

function q(over: Partial<QueryStatusLike> = {}): QueryStatusLike {
  return { data: {}, error: null, isPending: false, isFetching: false, ...over };
}

const broken = (id: "kpi" | "activity", label: string) =>
  aggregateRegion(id, label, [q({ data: null, error: new Error(`${label} unreachable`) })]);

describe("SourceAlerts", () => {
  afterEach(cleanup);

  it("renders one alert per failed source, naming the source and its error", () => {
    render(
      <SourceAlerts
        regions={[broken("kpi", "KPI summary"), broken("activity", "Recent activity")]}
        onRetry={() => {}}
      />,
    );
    const alerts = screen.getAllByRole("alert");
    expect(alerts).toHaveLength(2);
    expect(alerts[0].textContent).toContain("KPI summary");
    expect(alerts[0].textContent).toContain("KPI summary unreachable");
    expect(alerts[1].textContent).toContain("Recent activity");
  });

  it("keeps the retry control out of the alerted elements", () => {
    render(<SourceAlerts regions={[broken("kpi", "KPI summary")]} onRetry={() => {}} />);
    // The button is a sibling of the alert, never announced as part of it.
    expect(screen.getByRole("alert").contains(screen.getByRole("button", { name: "Retry" }))).toBe(
      false,
    );
  });

  it("renders nothing when every source is healthy", () => {
    const healthy = aggregateRegion("kpi", "KPI summary", [q()]);
    const { container } = render(<SourceAlerts regions={[healthy]} onRetry={() => {}} />);
    expect(container.innerHTML).toBe("");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("retries every source when the button is pressed", () => {
    let hits = 0;
    render(
      <SourceAlerts
        regions={[broken("kpi", "KPI summary"), broken("activity", "Recent activity")]}
        onRetry={() => {
          hits += 1;
        }}
      />,
    );
    screen.getByRole("button", { name: "Retry" }).click();
    expect(hits).toBe(1);
  });
});
