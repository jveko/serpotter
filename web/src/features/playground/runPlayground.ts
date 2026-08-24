import { apiBase } from "@/lib/api";
import { PLAY_TOKEN_KEY } from "@/lib/constants";
import { playgroundHttpError } from "./errors";

export type RunPlaygroundArgs = {
  token: string;
  mode?: string;
  query?: string;
  maxResults?: number | string;
  url?: string;
  scrapeTopN?: number | string;
};

export type RunPlaygroundResult =
  | { ok: true; status: number; data: unknown }
  | { ok: false; status: number | null; error: string };

export type PlaygroundRequest = { path: string; body: Record<string, unknown> };

/** Builds the endpoint + body for a playground mode (pure — no I/O). */
export function buildPlaygroundRequest(args: RunPlaygroundArgs): PlaygroundRequest {
  const m =
    String(args.mode ?? "search")
      .trim()
      .toLowerCase() || "search";
  if (m === "extract") {
    return { path: "/api/extract", body: { url: String(args.url ?? "").trim() } };
  }
  if (m === "research") {
    const body: Record<string, unknown> = { query: String(args.query ?? "").trim() };
    const maxN = Number(args.maxResults);
    if (Number.isFinite(maxN) && maxN > 0) body.maxResults = maxN;
    const scrapeN = Number(args.scrapeTopN);
    if (Number.isFinite(scrapeN) && scrapeN >= 0 && String(args.scrapeTopN ?? "").trim() !== "") {
      body.scrapeTopN = scrapeN;
    }
    return { path: "/api/research", body };
  }
  return {
    path: "/api/search",
    body: {
      query: String(args.query ?? "").trim(),
      maxResults: Number(args.maxResults) || 5,
    },
  };
}

export async function runPlayground(args: RunPlaygroundArgs): Promise<RunPlaygroundResult> {
  const { path, body } = buildPlaygroundRequest(args);
  try {
    const res = await fetch(`${apiBase()}${path}`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${String(args.token ?? "").trim()}`,
        "content-type": "application/json",
      },
      body: JSON.stringify(body),
    });
    const text = await res.text();
    let data: unknown;
    try {
      data = text ? JSON.parse(text) : null;
    } catch {
      data = text;
    }
    if (!res.ok) {
      return {
        ok: false,
        status: res.status,
        error: playgroundHttpError(res, data, text),
      };
    }
    // Persisting the token is best-effort: a storage failure (quota, disabled
    // storage) must never flip a successful response into a reported failure.
    try {
      localStorage.setItem(PLAY_TOKEN_KEY, String(args.token ?? "").trim());
    } catch {
      // ignore — the request already succeeded
    }
    return { ok: true, status: res.status, data };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return { ok: false, status: null, error: msg };
  }
}
