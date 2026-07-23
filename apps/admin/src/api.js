import { SESSION_KEY } from "./constants.js";

export function apiBase() {
  return import.meta.env.VITE_API_BASE || "";
}

export async function parseJsonResponse(res) {
  const text = await res.text();
  let data = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    data = text;
  }
  if (!res.ok) {
    const msg =
      (typeof data === "object" && data && (data.detail || data.title)) ||
      res.statusText ||
      "request failed";
    throw new Error(typeof msg === "string" ? msg : String(msg));
  }
  return data;
}

export async function adminFetch(path, secret, opts = {}) {
  const session =
    typeof localStorage !== "undefined"
      ? localStorage.getItem(SESSION_KEY)
      : null;
  const bearer = session || secret;
  const headers = {
    ...(opts.headers || {}),
    Authorization: `Bearer ${bearer}`,
  };
  if (opts.body && !headers["content-type"]) {
    headers["content-type"] = "application/json";
  }
  const res = await fetch(`${apiBase()}${path}`, { ...opts, headers });
  return parseJsonResponse(res);
}
