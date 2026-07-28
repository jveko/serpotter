import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/_auth/playground")({
  component: PlaygroundStub,
});

function PlaygroundStub() {
  return (
    <section className="panel" id="playground">
      <div className="panel__head">
        <h2 className="panel__title">API playground</h2>
        <span className="panel__meta">stub</span>
      </div>
      <div className="panel__body">
        <p className="muted">Coming soon</p>
      </div>
    </section>
  );
}
