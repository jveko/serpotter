import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/_auth/keys")({
  component: KeysStub,
});

function KeysStub() {
  return (
    <section className="panel" id="keys">
      <div className="panel__head">
        <h2 className="panel__title">Provider keys</h2>
        <span className="panel__meta">stub</span>
      </div>
      <div className="panel__body">
        <p className="muted">Coming soon</p>
      </div>
    </section>
  );
}
