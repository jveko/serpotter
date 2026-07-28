import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/_auth/settings")({
  component: SettingsStub,
});

function SettingsStub() {
  return (
    <section className="panel" id="settings">
      <div className="panel__head">
        <h2 className="panel__title">Settings</h2>
        <span className="panel__meta">stub</span>
      </div>
      <div className="panel__body">
        <p className="muted">Coming soon</p>
      </div>
    </section>
  );
}
