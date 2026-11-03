// The inline capture syntax of crates/todo-core/src/capture.rs (design 2.3),
// for FakeDaemon's ParseCapture and CaptureItem. Token offsets are in Unicode
// code points, and "whitespace" is Unicode White_Space, as in Rust.

import { create } from "@bufbuild/protobuf";
import { DateSchema } from "./gen/google/type/date_pb.js";
import { PersonRefSchema } from "./gen/todo/v1/common_pb.js";
import { Energy, Priority, Status } from "./gen/todo/v1/item_pb.js";
import { type CaptureDraft, CaptureDraftSchema, CaptureTokenKind, CaptureTokenSchema } from "./gen/todo/v1/item_service_pb.js";
import { parseDateExpr, protoDate, rustTrim } from "./dates.js";
import { type RuleSpec, firstOccurrence, parseRule, resolve, ruleToProto } from "./recurrence.js";

const SIGILS = ["+", "^", "#", "~", ">", "@", "%"];

const WHITESPACE = /^\p{White_Space}$/u;
const WHITESPACE_RUNS = /\p{White_Space}+/u;
const CONTROL = /\p{Cc}/u;

/** What a successfully parsed token sets. */
type Effect =
  | { set: "priority"; priority: Priority }
  | { set: "dueDate"; day: number }
  | { set: "label"; label: string }
  | { set: "waiting"; name: string }
  | { set: "status"; status: Status }
  | { set: "energy"; energy: Energy }
  /** Resolved once the due date is known. */
  | { set: "recurrence"; spec: RuleSpec };

/** A title word, or a token of `kind` with what it sets or why it does not parse. */
type Piece = { title: string } | { kind: CaptureTokenKind; result: Effect | string };

/** A word of the text, as a code-point span. */
interface Word {
  start: number;
  end: number;
  /** A `~"…"` word closed by a quote. */
  quoted: boolean;
}

/**
 * Parses capture text against day `today`, as `todo_core::parse_capture`
 * does. Never fails: a token that does not parse stays in the title with a
 * warning, and text without any title words is kept whole as the title
 * with no token applied. A `~` token's waiting is free text, since `today`.
 */
export function parseCaptureText(text: string, today: number): CaptureDraft {
  const chars = Array.from(text);
  const draft = create(CaptureDraftSchema, { priority: Priority.NONE, status: Status.INBOX, energy: Energy.NONE });
  const title: string[] = [];
  const typed: string[] = [];
  const parsed: [number, Effect][] = [];
  for (const word of words(chars)) {
    const raw = chars.slice(word.start, word.end).join("");
    const piece = classify(raw, word.quoted, today);
    if ("title" in piece) {
      title.push(piece.title);
      continue;
    }
    let warning = "";
    if (typeof piece.result === "string") {
      title.push(collapseWhitespace(raw));
      warning = `${raw}: ${piece.result}`;
    } else {
      parsed.push([draft.tokens.length, piece.result]);
    }
    draft.tokens.push(create(CaptureTokenSchema, { start: word.start, end: word.end, kind: piece.kind, applied: warning === "", warning }));
    typed.push(raw);
  }

  const keptAsTyped = title.length === 0 && draft.tokens.length > 0;
  if (title.length === 0) {
    draft.title = collapseWhitespace(text);
    for (const [i, token] of draft.tokens.entries()) {
      token.applied = false;
      token.warning = `${typed[i]}: kept as title text`;
    }
  } else {
    draft.title = title.join(" ");
    applyEffects(draft, parsed, typed, today);
  }
  draft.warnings = draft.tokens.filter((t) => !t.applied).map((t) => t.warning);
  if (keptAsTyped) draft.warnings.push("no title words: the text was kept as typed");
  return draft;
}

/**
 * Applies parsed tokens (indexes into `draft.tokens`, with the text of each
 * token in `typed`). The last token of a single-valued kind wins; every
 * label applies, once. The rule is resolved last, against the winning due
 * date (else `today`), so `@weekly ^fri` reads as `^fri @weekly`.
 */
function applyEffects(draft: CaptureDraft, parsed: readonly [number, Effect][], typed: readonly string[], today: number): void {
  const winners = new Map<CaptureTokenKind, number>();
  for (const [i, effect] of parsed) if (effect.set !== "label") winners.set(draft.tokens[i]!.kind, i);
  const labels = new Set<string>();
  let due: number | undefined;
  let spec: RuleSpec | undefined;
  for (const [i, effect] of parsed) {
    const token = draft.tokens[i]!;
    const winner = winners.get(token.kind);
    if (winner !== undefined && winner !== i) {
      token.applied = false;
      token.warning = `${typed[i]}: overridden by ${typed[winner]}`;
      continue;
    }
    switch (effect.set) {
      case "priority":
        draft.priority = effect.priority;
        break;
      case "energy":
        draft.energy = effect.energy;
        break;
      case "dueDate":
        due = effect.day;
        draft.dueDate = create(DateSchema, protoDate(effect.day));
        break;
      case "label":
        if (!labels.has(effect.label)) {
          labels.add(effect.label);
          draft.labels.push(effect.label);
        }
        break;
      case "waiting":
        draft.waitingOn = create(PersonRefSchema, { ref: { case: "freeText", value: effect.name } });
        break;
      case "status":
        draft.status = effect.status;
        break;
      case "recurrence":
        spec = effect.spec;
        break;
    }
  }
  if (spec !== undefined) {
    const rule = resolve(spec, due ?? today);
    draft.recurrence = ruleToProto(rule);
    const first = firstOccurrence(rule, due, today);
    if (first !== undefined) draft.recurrenceFirst = create(DateSchema, protoDate(first));
  }
}

/**
 * Splits on whitespace, except that a word starting with `~"` runs to the
 * first later `"` followed by whitespace or the end of the text.
 */
function words(chars: readonly string[]): Word[] {
  // Quotes that can close a `~"` word, consumed as the words advance so that
  // unterminated quotes do not rescan the rest of the text.
  const closes: number[] = [];
  for (let j = 0; j < chars.length; j++) {
    if (chars[j] === '"' && (j + 1 === chars.length || isWhitespace(chars[j + 1]!))) closes.push(j);
  }
  let next = 0;
  const out: Word[] = [];
  let i = 0;
  while (i < chars.length) {
    if (isWhitespace(chars[i]!)) {
      i++;
      continue;
    }
    const start = i;
    let end = start;
    while (end < chars.length && !isWhitespace(chars[end]!)) end++;
    let quoted = false;
    if (chars[start] === "~" && chars[start + 1] === '"') {
      while (next < closes.length && closes[next]! < start + 2) next++;
      if (next < closes.length) {
        end = closes[next]! + 1;
        quoted = true;
      }
    }
    out.push({ start, end, quoted });
    i = end;
  }
  return out;
}

function classify(word: string, quoted: boolean, today: number): Piece {
  // Every sigil, and the escape, is one UTF-16 unit.
  const sigil = word[0]!;
  const rest = word.slice(1);
  if (sigil === "\\") return { title: SIGILS.includes(rest[0] ?? "") ? rest : word };
  if (!SIGILS.includes(sigil) || rest === "") return { title: word };
  switch (sigil) {
    case "+":
      return { kind: CaptureTokenKind.PRIORITY, result: priority(rest) };
    case "^": {
      const date = parseDateExpr(rest, today);
      return { kind: CaptureTokenKind.DUE_DATE, result: "day" in date ? { set: "dueDate", day: date.day } : date.reason };
    }
    case "#":
      return { kind: CaptureTokenKind.LABEL, result: CONTROL.test(rest) ? "labels must not contain control characters" : { set: "label", label: rest } };
    case "~":
      return { kind: CaptureTokenKind.WAITING, result: waiting(rest, quoted) };
    case ">":
      return { kind: CaptureTokenKind.STATUS, result: status(rest) };
    case "%":
      return { kind: CaptureTokenKind.ENERGY, result: energy(rest) };
    default: {
      // The whole word, so that `@@daily` is not a rule, as `^^fri` is not a date.
      const rule = parseRule(word);
      return { kind: CaptureTokenKind.RECURRENCE, result: "spec" in rule ? { set: "recurrence", spec: rule.spec } : rule.reason };
    }
  }
}

const PRIORITIES: Record<string, Priority> = {
  "1": Priority.HIGHEST,
  "2": Priority.HIGH,
  "3": Priority.MEDIUM,
  "4": Priority.LOW,
  "5": Priority.LOWEST,
};

function priority(digits: string): Effect | string {
  const p = Object.hasOwn(PRIORITIES, digits) ? PRIORITIES[digits] : undefined;
  return p === undefined ? "priority must be +1 to +5" : { set: "priority", priority: p };
}

/** `rest` follows the `~`; a quoted word still has both quotes. */
function waiting(rest: string, quoted: boolean): Effect | string {
  let name = rest;
  if (quoted) name = rustTrim(rest.slice(1, -1));
  else if (rest.startsWith('"')) return "missing closing quote";
  return name === "" ? "waiting needs a name" : { set: "waiting", name };
}

function status(word: string): Effect | string {
  // Rust's eq_ignore_ascii_case.
  const lower = word.replace(/[A-Z]/g, (c) => c.toLowerCase());
  if (lower === "next") return { set: "status", status: Status.NEXT };
  if (lower === "someday") return { set: "status", status: Status.SOMEDAY };
  return "status must be >next or >someday";
}

const ENERGIES: Record<string, Energy> = { low: Energy.LOW, med: Energy.MEDIUM, medium: Energy.MEDIUM, high: Energy.HIGH };

function energy(word: string): Effect | string {
  // Rust's to_ascii_lowercase.
  const lower = word.replace(/[A-Z]/g, (c) => c.toLowerCase());
  const e = Object.hasOwn(ENERGIES, lower) ? ENERGIES[lower] : undefined;
  return e === undefined ? "not an energy level: use %low, %med or %high" : { set: "energy", energy: e };
}

function isWhitespace(c: string): boolean {
  return WHITESPACE.test(c);
}

function collapseWhitespace(s: string): string {
  return s
    .split(WHITESPACE_RUNS)
    .filter((w) => w !== "")
    .join(" ");
}
