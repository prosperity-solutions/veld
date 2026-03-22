import { API } from "./constants";

/** Make a JSON API request to the feedback server. */
export function api(
  method: string,
  path: string,
  body?: unknown,
): Promise<unknown> {
  const opts: RequestInit = {
    method,
    headers: { "Content-Type": "application/json" },
  };
  if (body !== undefined) opts.body = JSON.stringify(body);
  return fetch(API + path, opts).then((r) => {
    if (!r.ok)
      throw new Error("API " + method + " " + path + " failed: " + r.status);
    if (r.status === 204) return null;
    return r.json();
  });
}
