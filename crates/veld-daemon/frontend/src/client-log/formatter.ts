const MAX_ARG_LEN = 8192; // 8KB per argument, prevent huge payloads

export function truncate(s: string): string {
  return s.length > MAX_ARG_LEN
    ? s.substring(0, MAX_ARG_LEN) + "...(truncated)"
    : s;
}

export function stringify(args: ArrayLike<unknown>): string {
  const parts: string[] = [];
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a === null) parts.push("null");
    else if (a === undefined) parts.push("undefined");
    else if (typeof a === "object") {
      try {
        parts.push(truncate(JSON.stringify(a)));
      } catch {
        parts.push(String(a));
      }
    } else parts.push(truncate(String(a)));
  }
  return parts.join(" ");
}

export function captureStack(scriptUrl: string): string {
  try {
    const s = new Error().stack || "";
    const lines = s.split("\n");
    const out: string[] = [];
    for (let j = 0; j < lines.length; j++) {
      const ln = lines[j];
      if (ln === "Error" || ln.indexOf(scriptUrl) >= 0) continue;
      out.push(ln);
    }
    return out.join("\n");
  } catch {
    return "";
  }
}
