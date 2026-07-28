import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/_auth/tokens")({
  component: TokensStub,
});

function TokensStub() {
  return (
    <section className="panel" id="tokens">
      <div className="panel__head">
        <h2 className="panel__title">API tokens</h2>
        <span className="panel__meta">stub</span>
      </div>
      <div className="panel__body">
        <p className="muted">Coming soon</p>
      </div>
    </section>
  );
}
