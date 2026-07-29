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

export async function runPlayground(
  args: RunPlaygroundArgs,
): Promise<RunPlaygroundResult> {
  const m = String(args.mode ?? "search").trim().toLowerCase() || "search";
  let path: string;
  let body: Record<string, unknown>;
  if (m === "extract") {
    path = "/api/extract";
    body = { url: String(args.url ?? "").trim() };
  } else if (m === "research") {
    path = "/api/research";
    body = { query: String(args.query ?? "").trim() };
    const maxN = Number(args.maxResults);
    if (Number.isFinite(maxN) && maxN > 0) body.maxResults = maxN;
    const scrapeN = Number(args.scrapeTopN);
    if (
      Number.isFinite(scrapeN) &&
      scrapeN >= 0 &&
      String(args.scrapeTopN ?? "").trim() !== ""
    ) {
      body.scrapeTopN = scrapeN;
    }
  } else {
    path = "/api/search";
    body = {
      query: String(args.query ?? "").trim(),
      maxResults: Number(args.maxResults) || 5,
    };
  }

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
    localStorage.setItem(PLAY_TOKEN_KEY, String(args.token ?? "").trim());
    return { ok: true, status: res.status, data };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return { ok: false, status: null, error: msg };
  }
}
