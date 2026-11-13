// Who `~name` names (design 2.3, 3.5), as the daemon reads it:
// crates/todo-core/src/person.rs, ported for FakeDaemon and for the TUI's
// `w` prompt, which says what enter saves before it is pressed.

import type { Person } from "./gen/todo/v1/person_pb.js";

/** Case and surrounding space folded away, as Rust's trim().to_lowercase(). */
const fold = (s: string): string => s.trim().toLowerCase();

/**
 * Everyone `text` names in the first tier that names anyone: a whole
 * username, email address or display name, else a display name's first
 * word.
 */
export function namedBy(text: string, people: Iterable<Person>): Person[] {
  const t = fold(text);
  if (!t) return [];
  const all = [...people];
  const whole = all.filter((p) => fold(p.username) === t || fold(p.displayName) === t || p.emails.some((e) => fold(e) === t));
  if (whole.length) return whole;
  return all.filter((p) => fold(p.displayName.trim().split(/\s+/)[0] ?? "") === t);
}

/** The one person `text` names; undefined for none or two and more. */
export function matchPerson(text: string, people: Iterable<Person>): Person | undefined {
  const found = namedBy(text, people);
  return found.length === 1 ? found[0] : undefined;
}

/**
 * Up to `limit` people for `query`, best first: a whole name, username or
 * email, then a word of one starting with it, then one containing it; by
 * display name within each (ListPeople's order).
 */
export function searchPeople(query: string, people: Iterable<Person>, limit = 50): Person[] {
  const q = fold(query);
  const rank = (p: Person): number | undefined => {
    if (!q) return 0;
    const keys = [fold(p.displayName), fold(p.username), ...p.emails.map(fold)];
    if (keys.includes(q)) return 0;
    if (keys.some((k) => k.split(/[\s.@_-]/).some((w) => w.startsWith(q)))) return 1;
    if (keys.some((k) => k.includes(q))) return 2;
    return undefined;
  };
  const cmp = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);
  return [...people]
    .flatMap((p) => {
      const r = rank(p);
      return r === undefined ? [] : [{ r, name: fold(p.displayName), p }];
    })
    .sort((a, b) => a.r - b.r || cmp(a.name, b.name) || cmp(a.p.id, b.p.id))
    .slice(0, limit)
    .map((x) => x.p);
}
