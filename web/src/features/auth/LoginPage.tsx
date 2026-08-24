import { useState, type FormEvent } from "react";
import { useNavigate, useRouter, useSearch } from "@tanstack/react-router";

import { secretProbeError, verifyAdminSecret } from "@/lib/api";
import { SECRET_KEY } from "@/lib/constants";
import { safeRedirectPath } from "@/lib/safe-redirect";

import { useAuth } from "./auth-context";

type LoginMode = "secret" | "password" | "bootstrap";

/**
 * Login / bootstrap gate. Owns mode + field state; applies auth then navigates.
 */
export function LoginPage() {
  const auth = useAuth();
  const router = useRouter();
  const navigate = useNavigate();
  const search = useSearch({ from: "/login" });

  const [mode, setMode] = useState<LoginMode>("secret");
  const [input, setInput] = useState(() => localStorage.getItem(SECRET_KEY) || "");
  const [loginUser, setLoginUser] = useState("admin");
  const [loginPass, setLoginPass] = useState("");
  const [bootstrapSecret, setBootstrapSecret] = useState(
    () => localStorage.getItem(SECRET_KEY) || "",
  );

  async function afterAuth() {
    await router.invalidate();
    await navigate({ to: safeRedirectPath(search.redirect) });
  }

  async function handleSecret(e: FormEvent) {
    e.preventDefault();
    const secret = input.trim();
    if (!secret) return;
    auth.setErr("");
    // Probe an admin endpoint first: an unset ADMIN_SECRET (503 AdminDisabled)
    // or a wrong secret (401) must keep the user on the gate with a clear
    // message instead of landing on a broken dashboard.
    const probe = await verifyAdminSecret(secret);
    const probeErr = secretProbeError(probe);
    if (probeErr) {
      auth.setErr(probeErr);
      return;
    }
    auth.applySecretToken(secret);
    await afterAuth();
  }

  async function handlePassword(e: FormEvent) {
    e.preventDefault();
    const username = loginUser.trim() || "admin";
    const password = loginPass;
    if (!password) return;
    let sess: { token: string; expiresAt: string };
    try {
      sess = await auth.loginWithPasswordHttp({ username, password });
    } catch {
      // auth.err already set; stay on gate
      return;
    }
    auth.applySessionToken(sess.token, sess.expiresAt);
    await afterAuth();
  }

  async function handleBootstrap(e: FormEvent) {
    e.preventDefault();
    const adminSecret = bootstrapSecret.trim();
    const password = loginPass;
    // Raw field (may be empty) so bootstrapHttp can omit body.username
    const username = loginUser.trim();
    if (!adminSecret || !password) return;
    let sess: { token: string; expiresAt: string };
    try {
      sess = await auth.bootstrapHttp({
        adminSecret,
        loginUser: username,
        password,
      });
    } catch {
      // auth.err already set; stay on gate
      return;
    }
    auth.applySessionToken(sess.token, sess.expiresAt);
    await afterAuth();
  }

  const busy = auth.busy;
  const err = auth.err;

  return (
    <div className="gate">
      <aside className="gate__aside">
        <span className="wordmark">
          Serpotter<span className="wordmark__dot">.</span>
        </span>
        <p className="gate__blurb">
          Search, extract and research API. This console manages client tokens, provider keys and
          outbound nodes.
        </p>
        <p className="kicker">Admin console</p>
      </aside>

      <main className="gate__main">
        <div className="gate__form-wrap">
          <div>
            <h1 className="gate__title">Sign in</h1>
            <p className="gate__lede">
              Authenticate with ADMIN_SECRET, an admin password session, or first-time bootstrap.
            </p>
          </div>

          <div className="seg" role="group" aria-label="Authentication method">
            <button
              type="button"
              className="seg__btn"
              aria-pressed={mode === "secret"}
              onClick={() => setMode("secret")}
            >
              ADMIN_SECRET
            </button>
            <button
              type="button"
              className="seg__btn"
              aria-pressed={mode === "password"}
              onClick={() => setMode("password")}
            >
              Password
            </button>
            <button
              type="button"
              className="seg__btn"
              aria-pressed={mode === "bootstrap"}
              onClick={() => setMode("bootstrap")}
            >
              Bootstrap
            </button>
          </div>

          {mode === "secret" && (
            <form onSubmit={handleSecret} className="gate__form">
              <p className="field__hint">
                Enter <span className="mono">ADMIN_SECRET</span> (Bearer or X-Admin-Password).
              </p>
              <div className="row">
                <label className="field field--grow">
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
                <label className="field field--grow">
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
                First-time setup: create admin user with <span className="mono">ADMIN_SECRET</span>,
                then session login.
              </p>
              <div className="row">
                <label className="field field--grow">
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
                <label className="field field--grow">
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

          {err && (
            <p className="err" role="alert">
              {err}
            </p>
          )}
        </div>
      </main>
    </div>
  );
}
