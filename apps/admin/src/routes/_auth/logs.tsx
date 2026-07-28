import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/_auth/logs")({
  component: LogsStub,
});

function LogsStub() {
  return (
    <section className="panel" id="logs">
      <div className="panel__head">
        <h2 className="panel__title">Request logs</h2>
        <span className="panel__meta">stub</span>
      </div>
      <div className="panel__body">
        <p className="muted">Coming soon</p>
      </div>
    </section>
  );
}
