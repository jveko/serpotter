import React, { useEffect } from "react";

import { SESSION_KEY } from "./constants.js";
import { useAdminSession } from "./hooks/useAdminSession.js";
import { useAdminData } from "./hooks/useAdminData.js";
import { useCmdk } from "./hooks/useCmdk.js";
import { LoginGate } from "./components/LoginGate.jsx";
import { Topbar } from "./components/Topbar.jsx";
import { Cmdk } from "./components/Cmdk.jsx";
import { StatsPanel } from "./components/panels/StatsPanel.jsx";
import { SettingsPanel } from "./components/panels/SettingsPanel.jsx";
import { TokensPanel } from "./components/panels/TokensPanel.jsx";
import { KeysPanel } from "./components/panels/KeysPanel.jsx";
import { NodesPanel } from "./components/panels/NodesPanel.jsx";
import { LogsPanel } from "./components/panels/LogsPanel.jsx";
import { PlaygroundPanel } from "./components/panels/PlaygroundPanel.jsx";

/**
 * Thin composition: session + data + cmdk → LoginGate or shell.
 * Business logic lives in hooks; panels own local form fields.
 */
export default function App() {
  const session = useAdminSession();
  const data = useAdminData(session.secret);
  const cmdk = useCmdk(session.loggedIn);

  // Mount / secret change safety net — refresh when secret set; clear auth on fail
  useEffect(() => {
    if (!session.secret) return;
    let cancelled = false;
    data.refresh(session.secret).catch(() => {
      if (!cancelled) session.clearAuth();
    });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- refresh/clearAuth stable via useCallback
  }, [session.secret]);

  async function handleSecret(secret) {
    const s = String(secret).trim();
    if (!s) return;
    try {
      // adminFetch prefers session over Bearer secret — probe without session
      localStorage.removeItem(SESSION_KEY);
      await data.refresh(s);
      session.applySecretToken(s);
    } catch {
      /* data.err set */
    }
  }

  async function handlePassword({ username, password }) {
    try {
      const token = await session.loginWithPasswordHttp({ username, password });
      session.applySessionToken(token);
      await data.refresh(token);
    } catch {
      session.clearAuth();
    }
  }

  async function handleBootstrap({ adminSecret, username, password }) {
    try {
      // LoginGate may pass empty username — map to loginUser for hook
      const token = await session.bootstrapHttp({
        adminSecret,
        loginUser: username,
        password,
      });
      session.applySessionToken(token);
      await data.refresh(token);
    } catch {
      session.clearAuth();
    }
  }

  function handleLogout() {
    session.logout();
    data.reset(); // must not clear PLAY_TOKEN_KEY / playToken storage
  }

  const busy = session.busy || data.busy;

  if (!session.loggedIn) {
    return (
      <LoginGate
        busy={busy}
        err={session.err || data.err}
        onSecret={handleSecret}
        onPassword={handlePassword}
        onBootstrap={handleBootstrap}
      />
    );
  }

  return (
    <div className="shell">
      <Topbar
        stats={data.stats}
        busy={busy}
        onRefresh={() => data.refresh(session.secret)}
        onLogout={handleLogout}
        onOpenCmdk={() => cmdk.setOpen(true)}
      />
      <main className="shell__main">
        {data.err && (
          <div className="banner" role="alert">
            <p className="banner__text err">{data.err}</p>
          </div>
        )}

        <div className="workbench">
          <StatsPanel stats={data.stats} />
          <SettingsPanel
            settings={data.settings}
            busy={busy}
            onSave={data.saveSettings}
          />
          <TokensPanel
            tokens={data.tokens}
            newToken={data.newToken}
            busy={busy}
            onCreate={data.createToken}
            onDelete={data.deleteToken}
            onUseInPlayground={data.useInPlayground}
          />
          <KeysPanel
            keys={data.keys}
            busy={busy}
            onCreate={data.createKey}
            onToggle={data.toggleKey}
            onDelete={data.deleteKey}
            onSyncCredits={data.syncCredits}
          />
          <NodesPanel
            nodes={data.nodes}
            busy={busy}
            onCreate={data.createNode}
            onToggle={data.toggleNode}
            onDelete={data.deleteNode}
          />
          <LogsPanel
            requestLogs={data.requestLogs}
            busy={busy}
            onRefresh={data.refreshLogsOnly}
          />
          <PlaygroundPanel
            playToken={data.playToken}
            onPlayTokenChange={data.setPlayToken}
            playResult={data.playResult}
            playErr={data.playErr}
            busy={busy}
            onSearch={data.runPlayground}
          />
        </div>

        <footer className="colophon">
          <p>Serpotter admin · Cobalt instrument panel</p>
        </footer>
      </main>

      {cmdk.open && (
        <Cmdk
          open={cmdk.open}
          query={cmdk.query}
          setQuery={cmdk.setQuery}
          index={cmdk.index}
          setIndex={cmdk.setIndex}
          filteredSections={cmdk.filteredSections}
          onClose={cmdk.close}
          onJump={cmdk.jumpTo}
          onKeyDown={cmdk.onCmdkKeyDown}
          inputRef={cmdk.inputRef}
        />
      )}
    </div>
  );
}
