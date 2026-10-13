/**
 * Keep the previous value's identity when a fresh one is structurally equal.
 *
 * The app's 5 s poll replaces `envs`, `repoList`, `shares`, `stats` and
 * `dbHealth` with objects freshly parsed from JSON, and nearly every tick the
 * contents are the same. A new identity is still a change as far as React is
 * concerned: every memo and effect keyed on the object re-runs. Returning the
 * old object from the state updater (`setX((prev) => keepIfEqual(prev, next))`)
 * makes React bail out of the render altogether.
 *
 * Compare the value that would actually be stored, not the raw response: a value
 * derived from the clock or from queued local patches has to be computed first,
 * or a change in either would be mistaken for "nothing new".
 *
 * Only JSON-shaped data is compared structurally (primitives, arrays, plain
 * objects). Anything else — a `Date`, a `Map`, a class instance — is equal only
 * to itself, which errs towards re-rendering rather than towards a stale view.
 */
export function keepIfEqual<T>(prev: T, next: T): T {
  return deepEqual(prev, next) ? prev : next;
}

export function deepEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) {
    return false;
  }
  const arrayA = Array.isArray(a);
  if (arrayA !== Array.isArray(b)) return false;
  if (arrayA) {
    const left = a as unknown[];
    const right = b as unknown[];
    if (left.length !== right.length) return false;
    for (let i = 0; i < left.length; i++) {
      if (!deepEqual(left[i], right[i])) return false;
    }
    return true;
  }
  if (!isPlain(a) || !isPlain(b)) return false;
  const left = a as Record<string, unknown>;
  const right = b as Record<string, unknown>;
  const keys = Object.keys(left);
  if (keys.length !== Object.keys(right).length) return false;
  for (const key of keys) {
    // `in` rather than a value check: `{x: undefined}` and `{}` differ to code
    // that tests for the key, and JSON never produces the former anyway.
    if (!Object.hasOwn(right, key) || !deepEqual(left[key], right[key])) return false;
  }
  return true;
}

function isPlain(value: object): boolean {
  const proto = Object.getPrototypeOf(value);
  return proto === Object.prototype || proto === null;
}
