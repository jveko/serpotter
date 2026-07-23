import React, { useState } from "react";

import { SECRET_KEY } from "../constants.js";

/**
 * Login / bootstrap gate. Owns mode + field state; parent handles auth.
 * Callbacks receive values (not events).
 */
export function LoginGate({ busy, err, onSecret, onPassword, onBootstrap }) {
  const [mode, setMode] = useState("secret"); // secret | password | bootstrap
  const [input, setInput] = useState(
    () => localStorage.getItem(SECRET_KEY) || "",
  );
  const [loginUser, setLoginUser] = useState("admin");
  const [loginPass, setLoginPass] = useState("");
  const [bootstrapSecret, setBootstrapSecret] = useState(
    () => localStorage.getItem(SECRET_KEY) || "",
  );

  function handleSecret(e) {
    e.preventDefault();
    const secret = input.trim();
    if (!secret) return;
    onSecret(secret);
  }

  function handlePassword(e) {
    e.preventDefault();
    const username = loginUser.trim() || "admin";
    const password = loginPass;
    if (!password) return;
    onPassword({ username, password });
  }

  function handleBootstrap(e) {
    e.preventDefault();
    const adminSecret = bootstrapSecret.trim();
    const password = loginPass;
    const username = loginUser.trim() || "admin";
    if (!adminSecret || !password) return;
    onBootstrap({ adminSecret, username, password });
  }

  return (
    <div className="gate">
      <div className="gate__card">
        <div className="gate__brand">
          <span className="wordmark">
            Serpotter<span className="wordmark__dot">.</span>
          </span>
          <p className="kicker">Admin console</p>
        </div>
        <h1 className="gate__title">Sign in</h1>
        <p className="gate__lede">
          Authenticate with ADMIN_SECRET, password session, or first-time
          bootstrap.
        </p>

        <div className="gate__row">
          <button
            type="button"
            className={
              mode === "secret"
                ? "btn btn--primary btn--sm"
                : "btn btn--secondary btn--sm"
            }
            onClick={() => setMode("secret")}
          >
            ADMIN_SECRET
          </button>
          <button
            type="button"
            className={
              mode === "password"
                ? "btn btn--primary btn--sm"
                : "btn btn--secondary btn--sm"
            }
            onClick={() => setMode("password")}
          >
            Password
          </button>
          <button
            type="button"
            className={
              mode === "bootstrap"
                ? "btn btn--primary btn--sm"
                : "btn btn--secondary btn--sm"
            }
            onClick={() => setMode("bootstrap")}
          >
            Bootstrap
          </button>
        </div>

        {mode === "secret" && (
          <form onSubmit={handleSecret} className="gate__form">
            <p className="field__hint">
              Enter <span className="mono">ADMIN_SECRET</span> (Bearer or
              X-Admin-Password).
            </p>
            <div className="row">
              <label className="field" style={{ flex: 1 }}>
                <span className="field__label">Secret</span>
                <input
                  className="input input--mono"
                  type="password"
                  value={input}
                  onChange={(e) => setInput(e.target.value)}
                  placeholder="ADMIN_SECRET"
                  autoComplete="current-password"
                />
              </label>
              <button
                type="submit"
                className="btn btn--primary"
                disabled={busy}
                data-state={busy ? "loading" : undefined}
              >
                Login
              </button>
            </div>
          </form>
        )}

        {mode === "password" && (
          <form onSubmit={handlePassword} className="gate__form">
            <p className="field__hint">
              Sign in with an admin user (after bootstrap). Stores{" "}
              <span className="mono">adm-</span> session token.
            </p>
            <div className="row">
              <label className="field">
                <span className="field__label">Username</span>
                <input
                  className="input input--narrow"
                  value={loginUser}
                  onChange={(e) => setLoginUser(e.target.value)}
                  placeholder="username"
                  autoComplete="username"
                />
              </label>
              <label className="field" style={{ flex: 1 }}>
                <span className="field__label">Password</span>
                <input
                  className="input"
                  type="password"
                  value={loginPass}
                  onChange={(e) => setLoginPass(e.target.value)}
                  placeholder="password"
                  autoComplete="current-password"
                />
              </label>
              <button
                type="submit"
                className="btn btn--primary"
                disabled={busy || !loginPass}
                data-state={busy ? "loading" : undefined}
              >
                Login
              </button>
            </div>
          </form>
        )}

        {mode === "bootstrap" && (
          <form onSubmit={handleBootstrap} className="gate__form">
            <p className="field__hint">
              First-time setup: create admin user with{" "}
              <span className="mono">ADMIN_SECRET</span>, then session login.
            </p>
            <div className="row">
              <label className="field" style={{ flex: 1 }}>
                <span className="field__label">ADMIN_SECRET</span>
                <input
                  className="input input--mono"
                  type="password"
                  value={bootstrapSecret}
                  onChange={(e) => setBootstrapSecret(e.target.value)}
                  placeholder="ADMIN_SECRET"
                />
              </label>
              <label className="field">
                <span className="field__label">Username</span>
                <input
                  className="input input--narrow"
                  value={loginUser}
                  onChange={(e) => setLoginUser(e.target.value)}
                  placeholder="username (opt)"
                />
              </label>
              <label className="field" style={{ flex: 1 }}>
                <span className="field__label">Password</span>
                <input
                  className="input"
                  type="password"
                  value={loginPass}
                  onChange={(e) => setLoginPass(e.target.value)}
                  placeholder="password"
                />
              </label>
              <button
                type="submit"
                className="btn btn--primary"
                disabled={busy || !bootstrapSecret.trim() || !loginPass}
                data-state={busy ? "loading" : undefined}
              >
                Bootstrap &amp; login
              </button>
            </div>
          </form>
        )}

        {err && <p className="err">{err}</p>}
      </div>
    </div>
  );
}
