import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/_auth/nodes")({
  component: NodesStub,
});

function NodesStub() {
  return (
    <section className="panel" id="nodes">
      <div className="panel__head">
        <h2 className="panel__title">Outbound nodes</h2>
        <span className="panel__meta">stub</span>
      </div>
      <div className="panel__body">
        <p className="muted">Coming soon</p>
      </div>
    </section>
  );
}
