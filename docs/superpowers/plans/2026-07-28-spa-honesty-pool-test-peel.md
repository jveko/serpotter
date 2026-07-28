# SPA Honesty + Pool Test Peel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:dispatching-parallel-agents for independent tasks to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface credit-sync success as a non-error notice, keep playground HTTP status on failures, and peel keypool/outbound unit tests into `src/tests.rs`.

**Architecture:** Two independent tracks. **C** is pure Rust module moves (no API change). **A** is admin SPA state: new `notice` alongside `err`, post-refresh sync messaging, playground status+chip on non-2xx. No backend/API changes.

**Tech Stack:** Existing Vite React admin SPA; Rust keypool/outbound crates; cargo test/clippy; `npm run build` for admin.

**Spec:** `docs/superpowers/specs/2026-07-28-spa-honesty-pool-test-peel-design.md`

## Global Constraints

- SPA package: **A1 + A2 only** (no advanced playground knobs)
- A1: **new `notice` state** in `useAdminData` — never put success text in `err`
- Clear `notice` wherever `setErr("")` runs; clear `notice` when setting `err` via `reportError` / partial
- Set success `notice` **after** `refresh` returns
- App sibling banner: `role="status"`, `.banner` / `.banner__text` **without** `.err`
- A2: always `setPlayStatus(res.status)` on completed HTTP; error chip uses existing **`chip--warn`**
- C: both keypool and outbound; body-only `tests.rs` under `#[cfg(test)] mod tests;` — no nested `mod tests`
- No `error.rs` peels; no Cargo.toml / public API changes; no wire/schema
- Never `git commit --no-verify`
- Prefer `rtk cargo test` / `rtk cargo clippy` when available

## File map

| File | Responsibility |
| --- | --- |
| `crates/serpotter-keypool/src/lib.rs` | Production only + `mod tests;` |
| `crates/serpotter-keypool/src/tests.rs` | Unit test body (moved) |
| `crates/serpotter-outbound/src/lib.rs` | Production only + `mod tests;` |
| `crates/serpotter-outbound/src/tests.rs` | Unit test body (moved) |
| `apps/admin/src/hooks/useAdminData.js` | `notice`; clear with err; A1 syncCredits; A2 runPlayground |
| `apps/admin/src/App.jsx` | Notice banner sibling |
| `apps/admin/src/components/panels/PlaygroundPanel.jsx` | Status chip on err path with `chip--warn` |

**Independent tracks:** Tasks 1–2 (C) do not share files with Tasks 3–5 (A). May run parallel via dispatching-parallel-agents if chosen; otherwise serial C then A is fine.

---

### Task 1: Peel keypool unit tests to `tests.rs`

**Files:**
- Create: `crates/serpotter-keypool/src/tests.rs`
- Modify: `crates/serpotter-keypool/src/lib.rs` (truncate after production; add `mod tests;`)

**Interfaces:**
- Consumes: existing production API in `lib.rs` (`KeyPool`, helpers via `super::*`)
- Produces: same 12 unit tests compiling as `keypool::tests::*`

- [ ] **Step 1: Cut the test module body into `tests.rs`**

In `crates/serpotter-keypool/src/lib.rs`, the block starts at:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // ... all tests through EOF
}
```

Create `crates/serpotter-keypool/src/tests.rs` containing **only the interior** (from `use super::*;` through the last test), **without** wrapping `mod tests { }`.

- [ ] **Step 2: Replace the inline module in `lib.rs` with**

```rust
#[cfg(test)]
mod tests;
```

Production code ends just before the old `#[cfg(test)]` (after `env_u64` / closing braces of helpers — currently ~line 203).

- [ ] **Step 3: Gate**

```bash
rtk cargo test -p serpotter-keypool
rtk cargo clippy -p serpotter-keypool -- -D warnings
```

Expected: all keypool unit tests pass (12); clippy clean.

- [ ] **Step 4: Commit**

```bash
rtk git add crates/serpotter-keypool/src/lib.rs crates/serpotter-keypool/src/tests.rs
rtk git commit -m "refactor(keypool): move unit tests to tests.rs"
```

---

### Task 2: Peel outbound unit tests to `tests.rs`

**Files:**
- Create: `crates/serpotter-outbound/src/tests.rs`
- Modify: `crates/serpotter-outbound/src/lib.rs`

**Interfaces:**
- Consumes: production `ProxyPool`, `proxy_url_from_node`, privates via `super::*`
- Produces: same outbound unit tests green

- [ ] **Step 1: Move test body**

Same recipe as Task 1. Inline block starts ~line 205:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    ...
}
```

`tests.rs` = interior only.

- [ ] **Step 2: End `lib.rs` with**

```rust
#[cfg(test)]
mod tests;
```

- [ ] **Step 3: Gate**

```bash
rtk cargo test -p serpotter-outbound
rtk cargo clippy -p serpotter-outbound -- -D warnings
```

Expected: all outbound unit tests pass; clippy clean.

- [ ] **Step 4: Commit**

```bash
rtk git add crates/serpotter-outbound/src/lib.rs crates/serpotter-outbound/src/tests.rs
rtk git commit -m "refactor(outbound): move unit tests to tests.rs"
```

---

### Task 3: A1 — `notice` state + credit-sync feedback + App banner

**Files:**
- Modify: `apps/admin/src/hooks/useAdminData.js`
- Modify: `apps/admin/src/App.jsx`

**Interfaces:**
- Consumes: existing `adminFetch` sync-credits report `{ synced, errors, results }`
- Produces: hook field `notice: string`; App renders status banner

- [ ] **Step 1: Add `notice` state next to `err`**

In `useAdminData.js` after `const [err, setErr] = useState("");`:

```javascript
const [notice, setNotice] = useState("");
```

- [ ] **Step 2: Clear `notice` with every `setErr("")` and on new errors**

Apply systematically via **grep**, not a guessed mutation list:

```bash
rg -n 'setErr\(' apps/admin/src/hooks/useAdminData.js
```

| Location | Change |
| --- | --- |
| `reportError` | `setNotice("");` then `setErr(...)` |
| Every existing `setErr("")` site | also `setNotice("")` on the same path (today: `refresh` start, `reset`, `createToken`, `createKey`, `syncCredits` start, `saveSettings`, `createNode`, `toggleNode`, `deleteNode`, `refreshLogsOnly`, …) |
| Partial sync `setErr(partialMsg)` | `setNotice("")` before/with `setErr` |

**Do not invent** new `setErr("")` clears on functions that do not clear err today (e.g. `toggleKey` / `deleteKey` currently omit start clears — leave that behavior; only pair sites that already call `setErr`).

- [ ] **Step 3: Rewrite `syncCredits` post-refresh messaging**

Replace the silent-success path. Keep fail/ok detail builders. After `await refresh(secret)`:

```javascript
const synced = Number(report?.synced ?? 0);
const errors = Number(report?.errors ?? 0);
// ... failed/ok detail strings as today for partial ...

await refresh(secret);

if (errors > 0) {
  setNotice("");
  setErr(
    `Credit sync partial: synced=${synced}, errors=${errors}${failDetail}${okDetail} (exa/xai soft-fail or fetch error; keys stay active)`,
  );
} else {
  setErr("");
  setNotice(`Credit sync: synced=${synced}, errors=0`);
}
```

At function start: `setBusy(true); setErr(""); setNotice("");`.

On catch: `reportError(e2)` only (reportError clears notice).

- [ ] **Step 4: Export `notice` from the hook return**

```javascript
return {
  busy,
  err,
  notice,
  setErr,
  // ...rest unchanged
};
```

- [ ] **Step 5: App.jsx sibling banner**

Immediately after the existing err banner block (~lines 121–125), add:

```jsx
{data.notice && (
  <div className="banner" role="status">
    <p className="banner__text">{data.notice}</p>
  </div>
)}
```

Do **not** add class `err` on the notice text. Reuse `.banner` / `.banner__text` only.

- [ ] **Step 6: Smoke-check logic (no browser required if unavailable)**

Re-read the file: every `setErr("")` has `setNotice("")`; success path never calls `setErr` with success text.

- [ ] **Step 7: Commit**

```bash
rtk git add apps/admin/src/hooks/useAdminData.js apps/admin/src/App.jsx
rtk git commit -m "feat(admin): credit-sync success notice banner"
```

---

### Task 4: A2 — Playground status + warn chip on errors

**Files:**
- Modify: `apps/admin/src/hooks/useAdminData.js` (`runPlayground`)
- Modify: `apps/admin/src/components/panels/PlaygroundPanel.jsx`

**Interfaces:**
- Consumes: raw `fetch` response + problem+json body
- Produces: `playStatus` set on all HTTP completions; `playErr` includes status; UI chip on err path

- [ ] **Step 1: Fix `runPlayground` response handling**

Replace the success-only `setPlayStatus` + `throw new Error(...)` HTTP-error path. Keep request build + `fetch` as today. After `res` returns:

```javascript
const text = await res.text();
let data;
try {
  data = text ? JSON.parse(text) : null;
} catch {
  data = text;
}

setPlayStatus(res.status);

if (!res.ok) {
  setPlayErr(playgroundHttpError(res, data, text));
  setPlayResult(null);
  return;
}

setPlayErr("");
setPlayResult(data);
localStorage.setItem(PLAY_TOKEN_KEY, String(token ?? "").trim());
```

Define this helper once in the same file (module scope or inside the callback — module scope preferred):

```javascript
function playgroundHttpError(res, data, text) {
  if (typeof data === "object" && data !== null) {
    const title = data.title != null ? String(data.title).trim() : "";
    const detail = data.detail != null ? String(data.detail).trim() : "";
    if (title && detail) return `${res.status} ${title}: ${detail}`;
    if (title) return `${res.status} ${title}`;
    if (detail) return `${res.status} ${detail}`;
  }
  const fallback =
    (typeof data === "string" && data) ||
    text ||
    res.statusText ||
    "request failed";
  return `${res.status} ${fallback}`;
}
```

Do **not** `throw` for HTTP errors (that historically skipped status). Network failures in outer `catch` still `setPlayErr(String(e2.message || e2))` without status (no response — acceptable).

- [ ] **Step 2: PlaygroundPanel chip on error path**

Today chip only renders inside `{playResult && (...)}`. Change so:

- If `playErr` and `playStatus != null`: show chip with `chip chip--warn` and label `` `${playStatus}` `` or `` `${playStatus} error` `` near the err line.
- If `playResult`: keep success chip `chip chip--ok` with `` `${playStatus} OK` `` when status present.

Example structure:

```jsx
{playErr && (
  <>
    {playStatus != null && (
      <span className="chip chip--warn">{playStatus} error</span>
    )}
    <p className="err">{playErr}</p>
  </>
)}
{playResult && (
  <div>
    <div className="pre__label">
      <span>response</span>
      <span className="chip chip--ok">
        {playStatus != null ? `${playStatus} OK` : "OK"}
      </span>
    </div>
    <pre className="pre mono">{JSON.stringify(playResult, null, 2)}</pre>
  </div>
)}
```

Match existing layout/spacing; do not invent new CSS beyond `chip--warn` (already in `styles.css`).

- [ ] **Step 3: Commit**

```bash
rtk git add apps/admin/src/hooks/useAdminData.js apps/admin/src/components/panels/PlaygroundPanel.jsx
rtk git commit -m "fix(admin): playground HTTP status on errors"
```

(If Task 3 already modified `useAdminData.js` uncommitted together, prefer one SPA commit only if both A1+A2 land same session — otherwise keep two commits as listed.)

---

### Task 5: Final gates

**Files:** none required unless build fails.

- [ ] **Step 1: Rust pools**

```bash
rtk cargo test -p serpotter-keypool -p serpotter-outbound
rtk cargo clippy -p serpotter-keypool -p serpotter-outbound -- -D warnings
```

Expected: pass / clean.

- [ ] **Step 2: Admin build**

```bash
cd apps/admin && npm run build
```

Expected: Vite build succeeds.

- [ ] **Step 3: Optional workspace rust** (if any doubt)

```bash
rtk cargo test --workspace
```

- [ ] **Step 4: Commit only if Step 2–3 required doc/path fixes; else done**

---

## Spec coverage

| Spec item | Task |
| --- | --- |
| C keypool tests.rs | Task 1 |
| C outbound tests.rs | Task 2 |
| A1 notice + clear rules + post-refresh + App banner | Task 3 |
| A2 status + err text + chip--warn | Task 4 |
| Gates cargo + npm build | Task 5 |
| No advanced playground / no API | Global |

## Plan self-review

1. Spec coverage: complete for A1+A2+C.
2. Placeholders: none; A2 error formatter given as single canonical helper.
3. Consistency: `notice` name matches App `data.notice`; `chip--warn` matches design.

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-28-spa-honesty-pool-test-peel.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks  
2. **Parallel Independent Domains** — Tasks 1–2 (C) can run parallel with Task 3 start only if different files; safest parallel split is **C (Tasks 1–2 serial or parallel with each other)** vs **A (Tasks 3–4)** as two domains, then Task 5 once

**Which approach?**
