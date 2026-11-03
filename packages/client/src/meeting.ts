// The Teams meeting link of crates/todo-core/src/meeting.rs (design 2.9),
// for FakeDaemon's BuildMeetingLink. test/conformance/meeting.ts checks
// the two against each other.

import type { Item } from "./gen/todo/v1/item_pb.js";
import { rustTrim } from "./dates.js";

export const TEAMS_NEW_MEETING = "https://teams.microsoft.com/l/meeting/new";

const MAX_CONTENT = 2000;

export interface Meeting {
  subject: string;
  content: string;
  attendees: string[];
}

/** Rust's char::is_whitespace (Unicode White_Space), which JS \s is not: it has U+0085 and not U+FEFF. */
const WHITESPACE = /[\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+/;

const singleLine = (s: string) => s.split(WHITESPACE).filter(Boolean).join(" ");

/** Rust's eq_ignore_ascii_case. */
const asciiLower = (s: string) => s.replace(/[A-Z]/g, (c) => c.toLowerCase());

/** Lines up to the first blank one, joined (Rust's str::trim and lines). */
function firstParagraph(md: string): string {
  const out: string[] = [];
  // Rust's str::lines: \n, and a \r before it.
  for (const line of rustTrim(md).split(/\r?\n/)) {
    if (rustTrim(line) === "") break;
    out.push(rustTrim(line));
  }
  return out.join(" ");
}

function isEmail(s: string): boolean {
  const parts = s.split("@");
  if (parts.length !== 2) return false;
  const [user, domain] = parts as [string, string];
  return user !== "" && domain.includes(".") && !domain.startsWith(".") && !domain.endsWith(".") && !/[\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000\p{Cc}]/u.test(s);
}

/** UTF-8 percent-encoding of all but the unreserved characters (RFC 3986). */
function encode(s: string): string {
  let out = "";
  for (const b of new TextEncoder().encode(s)) {
    const c = String.fromCharCode(b);
    out += /[A-Za-z0-9\-._~]/.test(c) ? c : `%${b.toString(16).toUpperCase().padStart(2, "0")}`;
  }
  return out;
}

/** Cuts `s` to at most `max` bytes of UTF-8, on a character boundary. */
function cutBytes(s: string, max: number): string {
  const enc = new TextEncoder();
  if (enc.encode(s).length <= max) return s;
  let out = "";
  let n = 0;
  for (const ch of s) {
    const len = enc.encode(ch).length;
    if (n + len > max) break;
    out += ch;
    n += len;
  }
  return `${out}…`;
}

export function meeting(items: readonly Item[], subject?: string): Meeting {
  const first = singleLine(items[0]?.title ?? "");
  const given = subject === undefined ? "" : singleLine(subject);
  const subj = given || (items.length > 1 ? `${first} + ${items.length - 1} more` : first);
  const parts: string[] = [];
  for (const item of items) {
    const part: string[] = [];
    if (items.length > 1) part.push(singleLine(item.title));
    const p = firstParagraph(item.description);
    if (p) part.push(p);
    const key = rustTrim(item.remoteLink?.key ?? "");
    const url = rustTrim(item.remoteLink?.url ?? "");
    if (key && url) part.push(`${key}: ${url}`);
    else if (key || url) part.push(key || url);
    if (part.length) parts.push(part.join("\n"));
  }
  const attendees: string[] = [];
  for (const item of items) {
    const on = item.waiting?.on?.ref;
    if (on?.case !== "freeText") continue;
    const addr = rustTrim(on.value);
    if (isEmail(addr) && !attendees.some((a) => asciiLower(a) === asciiLower(addr))) attendees.push(addr);
  }
  return { subject: subj, content: cutBytes(parts.join("\n\n"), MAX_CONTENT), attendees };
}

export function teamsUrl(m: Meeting): string {
  const params: [string, string][] = [["subject", m.subject]];
  if (m.content) params.push(["content", m.content]);
  if (m.attendees.length) params.push(["attendees", m.attendees.join(",")]);
  return `${TEAMS_NEW_MEETING}?${params.map(([k, v]) => `${k}=${encode(v)}`).join("&")}`;
}
