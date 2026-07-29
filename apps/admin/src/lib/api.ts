import { SECRET_KEY, SESSION_KEY } from "./constants";

export function apiBase(): string {
  return import.meta.env.VITE_API_BASE || "";
}

/** Prefer session token, then secret. */
export function getAdminBearer(): string | null {
  if (typeof localStorage === "undefined") return null;
  return localStorage.getItem(SESSION_KEY) || localStorage.getItem(SECRET_KEY) || null;
}

export class HttpError extends Error {
  status: number;

  constructor(message: string, status: number) {
    super(message);
    this.name = "HttpError";
    this.status = status;
  }
}

function problemMessage(data: unknown, fallback: string): string {
  if (typeof data === "object" && data !== null) {
    if ("detail" in data && data.detail != null) {
      return typeof data.detail === "string" ? data.detail : String(data.detail);
    }
    if ("title" in data && data.title != null) {
      return typeof data.title === "string" ? data.title : String(data.title);
    }
  }
  return fallback;
}

export async function parseJsonResponse<T>(res: Response): Promise<T> {
  const text = await res.text();
  let data: unknown = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    data = text;
  }
  if (!res.ok) {
    throw new HttpError(problemMessage(data, res.statusText || "request failed"), res.status);
  }
  return data as T;
}

export async function adminFetch<T>(
  path: string,
  opts: RequestInit & { bearer?: string | null } = {},
): Promise<T> {
  const { bearer: explicitBearer, headers: initHeaders, ...rest } = opts;
  const bearer = explicitBearer !== undefined ? explicitBearer : getAdminBearer();
  const headers: Record<string, string> = {
    ...(initHeaders as Record<string, string> | undefined),
  };
  if (bearer) {
    headers.Authorization = `Bearer ${bearer}`;
  }
  if (rest.body && !headers["content-type"] && !headers["Content-Type"]) {
    headers["content-type"] = "application/json";
  }
  const res = await fetch(`${apiBase()}${path}`, { ...rest, headers });
  return parseJsonResponse<T>(res);
}
