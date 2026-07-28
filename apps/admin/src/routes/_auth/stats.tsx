import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/_auth/stats")({
  component: StatsStub,
});

function StatsStub() {
  return (
    <section className="panel" id="stats">
      <div className="panel__head">
        <h2 className="panel__title">Stats</h2>
        <span className="panel__meta">stub</span>
      </div>
      <div className="panel__body">
        <p className="muted">Coming soon</p>
      </div>
    </section>
  );
}
