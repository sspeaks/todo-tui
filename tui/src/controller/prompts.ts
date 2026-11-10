// The prompts on the message line (Decision 5 and 6): i (title), t (labels),
// ^ and D (dates, with a ParseDate preview), w (waiting, with tab
// completion of the person) and @ (the repeat rule, with a ParseRecurrence
// preview). The Target is frozen when the prompt opens. Only i and t send
// expected_revision: their text was made from the rendered value, so a
// conflict rebases silently when that field is unchanged on the server,
// and otherwise keeps the text for enter to overwrite. With marks, t ^ D w @
// act on every marked Item in one BatchMutate (Decision 8): t then takes
// "+a -b", applied to each Item's own labels.

import {
  type Item,
  ItemField,
  type ItemSchema,
  type MessageInitShape,
  Operator,
  type ParseDateResponse,
  type ParseRecurrenceResponse,
  type UpdateItemResponse,
  type ProtoDate,
  ProtoDateSchema,
  type RecurrenceRule,
  RecurrenceRuleSchema,
  create,
  equals,
  errorReason,
  parseDate,
  parseRecurrence,
  setRecurrence,
  updateItem,
} from "@todo-tui/client";
import { civil, dayLabel, dayOf, isoDay } from "../format/dates.js";
import {
  applyLabelEdits,
  commonPrefix,
  formatLabels,
  parseLabelEdits,
  parseLabels,
  parseWaiting,
  sameLabels,
  waitingPrefix,
} from "../format/fields.js";
import { repeatPreview, ruleText } from "../format/recurrence.js";
import { singleLine } from "../format/text.js";
import type { PromptKind } from "../keys/keymap.js";
import { editLine, insert, type LineState, lineOf } from "../keys/line-editor.js";
import { isChar, type KeyToken } from "../keys/tokenize.js";
import { type Mode, normalMode, type PromptPreview, type Target } from "../state/types.js";
import { Bulk, type BulkStep, bulkText, stepsOf, updateStep } from "./bulk.js";
import type { Controller } from "./controller.js";
import { Latest } from "./latest.js";
import { errorText, opText, quoted } from "./messages.js";
import { type Outcome, single } from "./pipeline.js";

type Prompt = Extract<Mode, { kind: "prompt" }>;

/** What each prompt says before its text. */
export const PROMPT_LABELS: Readonly<Record<PromptKind, string>> = {
  title: "Title",
  labels: "Labels",
  due: "Due",
  defer: "Defer until",
  waiting: "Waiting on",
  repeat: "Repeats",
  meeting: "Meeting",
};

/** How many times a derived edit rebases silently before it shows the conflict. */
const MAX_REBASES = 3;

const sameDate = (a: ProtoDate | undefined, b: ProtoDate | undefined) =>
  a === undefined || b === undefined ? a === b : dayOf(a) === dayOf(b);

const sameRule = (a: RecurrenceRule | undefined, b: RecurrenceRule | undefined) =>
  a === undefined || b === undefined ? a === b : equals(RecurrenceRuleSchema, a, b);

/**
 * What anchors an Item's weekly or monthly: its due date, else the
 * daemon's `today`, so that the preview made before midnight is stale
 * after it.
 */
const anchorKey = (item: Item, today: number): string => (item.dueDate ? isoDay(dayOf(item.dueDate)) : `today ${isoDay(today)}`);

/** `weekly` or `monthly`, which take their day from the anchor; else undefined. */
const onAnchor = (text: string): string | undefined => /^@?\s*(weekly|monthly)$/i.exec(text.trim())?.[1]?.toLowerCase();

/**
 * What `@` sends ParseRecurrence as the anchor for `item`: its due date;
 * else, for weekly or monthly, the `today` the preview counted from, so
 * that past the daemon's midnight, before the midnight poll sees it,
 * enter still saves the day shown. Else none, and the daemon counts from
 * its own today (a 3d then has no first date).
 */
function anchorOf(item: Item | undefined, text: string, today: number): ProtoDate | undefined {
  if (item?.dueDate || !onAnchor(text)) return item?.dueDate;
  const { year, month, day } = civil(today);
  return create(ProtoDateSchema, { year, month, day });
}

/**
 * What `@` asks ParseRecurrence about: the text, the anchor, and whether
 * the marked Items' anchors differ; `key` names the anchor ("*" when they
 * differ), so a preview made for another one is known to be stale.
 */
interface RepeatQuery {
  readonly text: string;
  readonly anchor: ProtoDate | undefined;
  readonly mixed: boolean;
  readonly key: string;
}

/** The free text an Item waits on; a Person id (P2) as is. */
const waitingOn = (item: Item): string | undefined => item.waiting?.on?.ref.value;

/** `w`'s prefill: "dana ^2026-10-30". */
function waitingText(item: Item): string {
  const on = waitingOn(item);
  if (on === undefined) return "";
  const follow = item.waiting?.followUpOn;
  return follow ? `${on} ^${isoDay(dayOf(follow))}` : on;
}

/** The date text a prompt's line asks ParseDate about; undefined when there is none to ask. */
function dateText(kind: PromptKind, text: string): string | undefined {
  if (kind === "due" || kind === "defer") return text.trim() || undefined;
  if (kind === "waiting") return parseWaiting(text).date;
  return undefined;
}

/** The one value `of` gives every Item, else undefined: a bulk prompt's prefill. */
function shared(items: readonly Item[], of: (item: Item) => string): string | undefined {
  const values = new Set(items.map(of));
  return values.size === 1 ? [...values][0] : undefined;
}

export class Prompts {
  private readonly dates: Latest<{ kind: PromptKind; text: string }, ParseDateResponse | undefined>;
  private readonly repeats: Latest<RepeatQuery, ParseRecurrenceResponse | undefined>;
  /** The anchor key of the repeat preview last asked for, and of the one shown. */
  private repeatAsked: string | undefined;
  private repeatShown: string | undefined;
  private readonly bulk: Bulk;

  constructor(private readonly c: Controller) {
    this.bulk = new Bulk(c);
    this.dates = new Latest(
      ({ kind, text }) => {
        const date = dateText(kind, text);
        return date === undefined ? Promise.resolve(undefined) : parseDate(c.items, date, { timeoutMs: c.timing.rpcTimeoutMs });
      },
      ({ kind, text }, res) => {
        const { mode } = this.c.state;
        // A response for text since edited is stale: the newer request follows.
        if (mode.kind !== "prompt" || mode.prompt !== kind || mode.line.text !== text) return;
        this.c.setMode({ ...mode, preview: this.previewOf(kind, text, res, mode.target.ids.length) });
      },
    );
    this.repeats = new Latest(
      ({ text, anchor }) =>
        text.trim()
          ? parseRecurrence(c.items, text, { timeoutMs: c.timing.rpcTimeoutMs, ...(anchor && { anchor }) })
          : Promise.resolve(undefined),
      (query, res) => {
        const { mode } = this.c.state;
        // Stale too when the anchor moved (a due date changed elsewhere): sync asked again.
        if (mode.kind !== "prompt" || mode.prompt !== "repeat" || mode.line.text !== query.text) return;
        if (this.repeatQuery(mode).key !== query.key) return;
        this.repeatShown = query.key;
        this.c.setMode({ ...mode, preview: this.repeatPreviewOf(query, res, mode.target.ids.length) });
      },
    );
  }

  /**
   * Opens a prompt on the selected Item, prefilled with the field's value.
   * `w` reads the people already waited on first, so that a tab typed
   * right after it completes.
   */
  open(kind: PromptKind): void | Promise<void> {
    if (kind === "meeting") return this.c.meetings.start();
    // The title is the selected Item's own; the rest act on the marks when there are some.
    const target = this.c.target(kind !== "title");
    const item = target && this.c.store.getItem(target.ids[0] ?? "");
    if (!target || !item) return;
    if (target.bulk) return this.openBulk(kind, target);
    let text = "";
    let base: Prompt["base"];
    switch (kind) {
      case "title":
        text = item.title;
        base = { value: item.title, revision: item.revision };
        break;
      case "labels":
        // A space after the last label, so the next one can be typed at once.
        text = item.labels.length ? `${formatLabels(item.labels)} ` : "";
        base = { value: formatLabels(item.labels), revision: item.revision };
        break;
      case "due":
        text = item.dueDate ? isoDay(dayOf(item.dueDate)) : "";
        break;
      case "defer":
        text = item.deferUntil ? isoDay(dayOf(item.deferUntil)) : "";
        break;
      case "waiting":
        text = waitingText(item);
        break;
      case "repeat":
        text = ruleText(item.recurrence?.rule);
        break;
    }
    this.c.setMode({ kind: "prompt", prompt: kind, target, line: lineOf(text), ...(base && { base }) });
    this.preview();
    if (kind === "waiting") return this.loadPeople(target);
  }

  /**
   * The bulk prompt: empty for t ("+a -b"); for a date, waiting or the
   * repeat rule, the value the marked Items share, if they share one.
   */
  private openBulk(kind: PromptKind, target: Target): void | Promise<void> {
    const items = this.bulk.items(target);
    let text: string | undefined;
    if (kind === "due") text = shared(items, (i) => (i.dueDate ? isoDay(dayOf(i.dueDate)) : ""));
    if (kind === "defer") text = shared(items, (i) => (i.deferUntil ? isoDay(dayOf(i.deferUntil)) : ""));
    if (kind === "waiting") text = shared(items, waitingText);
    if (kind === "repeat") text = shared(items, (i) => ruleText(i.recurrence?.rule));
    this.c.setMode({ kind: "prompt", prompt: kind, target, line: lineOf(text ?? "") });
    this.preview();
    if (kind === "waiting") return this.loadPeople(target);
  }

  key(mode: Prompt, key: KeyToken): void | Promise<void> {
    switch (key) {
      case "esc":
      case "ctrl+c":
        return this.c.setMode(normalMode);
      case "enter":
        return this.submit(mode);
      case "tab":
        if (mode.prompt === "waiting") this.complete(mode);
        return;
    }
    const line = isChar(key) ? insert(mode.line, this.c.charRun(key)) : editLine(mode.line, key);
    if (line) this.edit(mode, line);
  }

  paste(mode: Prompt, text: string): void {
    this.edit(mode, insert(mode.line, singleLine(text)));
  }

  private edit(mode: Prompt, line: LineState): void {
    this.c.setMode({ ...mode, line, error: undefined });
    this.preview();
  }

  /** Asks for the preview of the current text: at once when no request is in flight, else after it. */
  private preview(): void {
    const { mode } = this.c.state;
    if (mode.kind !== "prompt" || mode.prompt === "title" || mode.prompt === "labels" || mode.prompt === "meeting") return;
    if (mode.prompt === "repeat") {
      const query = this.repeatQuery(mode);
      this.repeatAsked = query.key;
      return this.repeats.request(query);
    }
    this.dates.request({ kind: mode.prompt, text: mode.line.text });
  }

  /**
   * After a store change: the repeat preview again when the anchor it was
   * asked for moved, so that weekly and monthly show the day enter saves.
   */
  sync(): void {
    const { mode } = this.c.state;
    if (mode.kind === "prompt" && mode.prompt === "repeat" && this.repeatQuery(mode).key !== this.repeatAsked) this.preview();
  }

  /**
   * The anchor `@`'s preview resolves weekly and monthly against: the
   * Item's due date, else today; with marks, the due date they share, else
   * today, and the first date is left out, since each counts from its own.
   */
  private repeatQuery(mode: Prompt): RepeatQuery {
    const items = this.bulk.items(mode.target);
    const today = this.c.today();
    const keys = new Set(items.map((i) => anchorKey(i, today)));
    const mixed = keys.size > 1;
    const { text } = mode.line;
    return { text, anchor: mixed ? undefined : anchorOf(items[0], text, today), mixed, key: mixed ? "*" : ([...keys][0] ?? "") };
  }

  /**
   * enter on weekly or monthly whose preview was made for another anchor
   * than `key`, the one it saves against (a due date that moved, an Item
   * read back with one, or the today of an Item with none): shows the
   * preview for it and keeps the prompt open, so what enter saves is what
   * was shown. Else false.
   */
  private anchorMoved(mode: Prompt, key: string): boolean {
    const latest = this.latest(mode);
    if (!onAnchor(latest.line.text) || latest.preview?.for !== latest.line.text || this.repeatShown === key) return false;
    this.c.setMode({ ...latest, preview: undefined });
    this.c.dropQueued();
    this.preview();
    return true;
  }

  /** Resolves once the preview has caught up with the text (tests). */
  async previewIdle(): Promise<void> {
    await Promise.all([this.dates.idle(), this.repeats.idle()]);
  }

  private previewOf(kind: PromptKind, text: string, res: ParseDateResponse | undefined, bulk = 1): PromptPreview {
    const today = this.c.today();
    const of = bulk > 1 ? ` of ${bulk} items` : "";
    const date = res?.date ? dayLabel(dayOf(res.date), today) : undefined;
    const warn = (t: string): PromptPreview => ({ for: text, text: t, warn: true });
    const info = (t: string): PromptPreview => ({ for: text, text: t, warn: false });
    if (kind === "waiting") {
      const w = parseWaiting(text);
      if (!w.person && w.date === undefined) return info(`enter clears waiting${bulk > 1 ? ` on ${bulk} items` : ""}`);
      if (!w.person) return warn("who is it waiting on? type a name before the ^date");
      if (w.date === undefined) return info(`→ ⧗${singleLine(w.person)}`);
      return date ? info(`→ ⧗${singleLine(w.person)} · follow up ${date}`) : warn(singleLine(res?.warning ?? ""));
    }
    if (!text.trim()) return info(`enter clears the ${kind === "due" ? "due" : "defer"} date${of}`);
    return date ? info(`→ ${date}`) : warn(singleLine(res?.warning ?? ""));
  }

  /**
   * "→ every Mon, Thu · first Mon Nov 2", or the warning. With marks due on
   * different days the first date is left out, and weekly or monthly says
   * that each takes its own day.
   */
  private repeatPreviewOf({ text, mixed }: RepeatQuery, res: ParseRecurrenceResponse | undefined, bulk: number): PromptPreview {
    const info = (t: string): PromptPreview => ({ for: text, text: t, warn: false });
    if (!res) return info(`enter stops repeating${bulk > 1 ? ` on ${bulk} items` : ""}`);
    if (!res.rule || res.warning) return { for: text, text: repeatPreview(res, this.c.today()), warn: true };
    if (!mixed) return info(repeatPreview(res, this.c.today()));
    const own = onAnchor(text);
    if (own) return info(`→ ${own}, each on its own due date's ${own === "weekly" ? "weekday" : "day"} (else today's)`);
    return info(repeatPreview({ ...res, first: undefined }, this.c.today()));
  }

  /** The people already waited on, for tab: one ListItems over every Item waiting. */
  private async loadPeople(target: Target): Promise<void> {
    let people: string[];
    try {
      const res = await this.c.items.listItems(
        {
          filter: {
            allOf: [{ kind: { case: "predicate", value: { target: ItemField.WAITING, op: Operator.IS_SET } } }],
            includeDone: true,
            includeArchived: true,
            includeDeferred: true,
          },
          pageSize: 1000,
        },
        { timeoutMs: this.c.timing.rpcTimeoutMs },
      );
      const names = new Map<string, string>();
      for (const item of res.items) {
        const on = item.waiting?.on?.ref;
        // One spelling per person, the first met.
        const key = on?.case === "freeText" ? on.value.trim().toLowerCase() : "";
        if (on?.case === "freeText" && key && !names.has(key)) names.set(key, on.value.trim());
      }
      people = [...names.values()].sort((a, b) => a.localeCompare(b, undefined, { sensitivity: "base" }));
    } catch {
      return;
    }
    const { mode } = this.c.state;
    if (mode.kind === "prompt" && mode.prompt === "waiting" && mode.target === target) this.c.setMode({ ...mode, people });
  }

  /** tab: completes the person from the people already waited on, as far as they agree. */
  private complete(mode: Prompt): void {
    const typed = waitingPrefix(mode.line.text);
    const after = mode.line.text.slice(typed.length);
    const prefix = typed.trim().toLowerCase();
    const matches = (mode.people ?? []).filter((p) => p.toLowerCase().startsWith(prefix));
    if (!matches.length) return;
    const one = matches.length === 1;
    const person = one ? (matches[0] ?? "") : commonPrefix(matches);
    if (!one && person.length <= prefix.length) return;
    const head = one && !after ? `${person} ` : person;
    this.edit(mode, { text: head + after, cursor: [...head].length });
  }

  // --- enter ---

  private submit(mode: Prompt): void | Promise<void> {
    if (mode.prompt === "meeting") return this.c.meetings.submit(mode);
    if (mode.target.bulk) return this.saveBulk(mode);
    const id = mode.target.ids[0] ?? "";
    const item = this.c.store.getItem(id);
    return item ? this.save(mode, item) : this.saveFetched(mode, id);
  }

  /**
   * enter on an Item the store no longer holds (a resync keeps only what
   * the views hold): read it, and save to it if it is still there; else the
   * prompt stays with the text, since there is nothing to save it to.
   */
  private async saveFetched(mode: Prompt, id: string): Promise<void> {
    let item: Item | undefined;
    try {
      item = await this.c.store.fetchItem(id);
    } catch (err) {
      return this.c.failed(errorText(err));
    }
    if (!item) return this.refuse(mode, "The Item was deleted elsewhere");
    return this.save(mode, item);
  }

  private save(mode: Prompt, item: Item): void | Promise<void> {
    switch (mode.prompt) {
      case "title":
        return this.saveTitle(mode, item);
      case "labels":
        return this.saveLabels(mode, item);
      case "due":
      case "defer":
        return this.saveDate(mode, item);
      case "waiting":
        return this.saveWaiting(mode, item);
      case "repeat":
        return this.saveRepeat(mode, item);
    }
  }

  /** Keeps the prompt open with why enter did not save. */
  private refuse(mode: Prompt, error: string): void {
    this.c.setMode({ ...this.latest(mode), error });
    this.c.dropQueued();
  }

  /** The prompt as it is now (a preview may have landed during an await), else as it was. */
  private latest(mode: Prompt): Prompt {
    const now = this.c.state.mode;
    return now.kind === "prompt" && now.target === mode.target ? now : mode;
  }

  private saveTitle(mode: Prompt, item: Item): void | Promise<void> {
    const title = mode.line.text.trim();
    if (!title) return this.refuse(mode, "The title cannot be empty");
    if (title === mode.base?.value) return this.c.setMode(normalMode);
    return this.saveDerived(mode, item, { id: item.id, title }, ["title"], (i) => i.title);
  }

  private saveLabels(mode: Prompt, item: Item): void | Promise<void> {
    const labels = parseLabels(mode.line.text);
    if (sameLabels(labels, parseLabels(mode.base?.value ?? ""))) return this.c.setMode(normalMode);
    return this.saveDerived(mode, item, { id: item.id, labels }, ["labels"], (i) => formatLabels(i.labels), (a, b) =>
      sameLabels(parseLabels(a), parseLabels(b)),
    );
  }

  /**
   * An edit made from the rendered value, with expected_revision. On a
   * conflict it reads the Item: if the field still holds the value the
   * edit started from, only other fields changed, and it retries on the
   * new revision; otherwise the prompt stays, with the server's value,
   * and the next enter overwrites it (Decision 6).
   */
  private async saveDerived(
    mode: Prompt,
    item: Item,
    patch: MessageInitShape<typeof ItemSchema> & { id: string },
    paths: readonly string[],
    valueOf: (item: Item) => string,
    same: (a: string, b: string) => boolean = (a, b) => a === b,
  ): Promise<void> {
    let base = mode.base ?? { value: valueOf(item), revision: item.revision };
    // A yes to someone else's issue holds for the rebases of this edit.
    let confirmed = false;
    for (let rebases = 0; ; rebases++) {
      const expectedRevision = base.revision;
      const out: Outcome<UpdateItemResponse> = await this.c.pipeline.run({
        call: (opts) => updateItem(this.c.items, patch, paths, { ...opts, expectedRevision }),
        items: single,
        confirmed,
      });
      confirmed = out.confirmed;
      if (out.ok) {
        this.c.setMode(normalMode);
        return this.c.toastIfLeft(item.id, out.res.result?.opId, opText("edit", patch.title ?? item.title));
      }
      if (errorReason(out.err) !== "REVISION_MISMATCH") return this.c.failed(errorText(out.err, item));
      let fresh: Item | undefined;
      try {
        fresh = await this.c.store.fetchItem(item.id);
      } catch (err) {
        return this.c.failed(errorText(err, item));
      }
      if (!fresh || fresh.deletedAt) return this.c.failed(`${quoted(item.title)} was deleted elsewhere`);
      const value = valueOf(fresh);
      if (same(value, base.value) && rebases < MAX_REBASES) {
        base = { value: base.value, revision: fresh.revision };
        continue;
      }
      this.c.setMode({ ...this.latest(mode), base: { value, revision: fresh.revision }, conflict: value });
      return this.c.dropQueued();
    }
  }

  /** ^ and D: the text through ParseDate; empty clears the date. */
  private async saveDate(mode: Prompt, item: Item): Promise<void> {
    const due = mode.prompt === "due";
    const text = mode.line.text.trim();
    let date: ProtoDate | undefined;
    if (text) {
      const res = await this.parse(mode, item, text);
      if (!res) return;
      date = res;
    }
    if (sameDate(date, due ? item.dueDate : item.deferUntil)) return this.c.setMode(normalMode);
    const patch = due ? { id: item.id, dueDate: date } : { id: item.id, deferUntil: date };
    const out = await this.c.pipeline.run({ call: (opts) => updateItem(this.c.items, patch, [due ? "due_date" : "defer_until"], opts), items: single });
    if (!out.ok) return this.c.failed(errorText(out.err, item));
    this.c.setMode(normalMode);
    // Deferring takes the Item out of every active view: always toasted.
    if (due) this.c.toastIfLeft(item.id, out.res.result?.opId, opText("edit", item.title));
    else this.c.toastOp(out.res.result?.opId, opText("edit", item.title));
  }

  /**
   * w: "<person> [^date]". The same person as now changes only the
   * follow-up (keeping since, the age the Waiting view sorts on); another
   * person replaces waiting, since then today. Empty clears it.
   */
  private async saveWaiting(mode: Prompt, item: Item): Promise<void> {
    const { person, date: dateText } = parseWaiting(mode.line.text);
    const current = item.waiting;
    if (!person) {
      if (dateText !== undefined) return this.refuse(mode, "Who is it waiting on? Type a name before the ^date");
      if (!current) return this.c.setMode(normalMode);
      return this.saveWaitingPatch(item, { id: item.id }, ["waiting"]);
    }
    let followUpOn: ProtoDate | undefined;
    if (dateText !== undefined) {
      followUpOn = await this.parse(mode, item, dateText);
      if (!followUpOn) return;
    }
    if (current && waitingOn(item) === person) {
      if (sameDate(followUpOn, current.followUpOn)) return this.c.setMode(normalMode);
      // The whole value, so that a daemon that copies the parent on a leaf path agrees.
      const waiting = { on: current.on, since: current.since, reason: current.reason, followUpOn };
      return this.saveWaitingPatch(item, { id: item.id, waiting }, ["waiting.follow_up_on"]);
    }
    const waiting = { on: { ref: { case: "freeText" as const, value: person } }, ...(followUpOn && { followUpOn }) };
    return this.saveWaitingPatch(item, { id: item.id, waiting }, ["waiting"]);
  }

  private async saveWaitingPatch(item: Item, patch: MessageInitShape<typeof ItemSchema> & { id: string }, paths: string[]): Promise<void> {
    const out = await this.c.pipeline.run({ call: (opts) => updateItem(this.c.items, patch, paths, opts), items: single });
    if (!out.ok) return this.c.failed(errorText(out.err, item));
    this.c.setMode(normalMode);
    this.c.toastIfLeft(item.id, out.res.result?.opId, opText("edit", item.title));
  }

  /**
   * @: the text through ParseRecurrence, anchored on the Item's due date;
   * empty stops the repeat. SetRecurrence is absolute: no expected_revision.
   */
  private async saveRepeat(mode: Prompt, item: Item): Promise<void> {
    const text = mode.line.text.trim();
    const today = this.c.today();
    if (this.anchorMoved(mode, anchorKey(item, today))) return;
    let rule: RecurrenceRule | undefined;
    if (text) {
      rule = await this.parseRepeat(mode, item, text, anchorOf(item, text, today));
      if (!rule) return;
    }
    if (rule ? sameRule(rule, item.recurrence?.rule) : !item.recurrence) return this.c.setMode(normalMode);
    const out = await this.c.pipeline.run({ call: (opts) => setRecurrence(this.c.items, item.id, rule, opts), items: single });
    if (!out.ok) return this.c.failed(errorText(out.err, item));
    this.c.setMode(normalMode);
    this.c.toastIfLeft(item.id, out.res.result?.opId, opText(rule ? "set repeat" : "stop repeating", item.title));
  }

  // --- with marks ---

  /**
   * enter on a bulk prompt: one step per marked Item it changes, in one
   * BatchMutate; nothing to change closes the prompt with a word.
   */
  private async saveBulk(mode: Prompt): Promise<void> {
    const items = this.bulk.items(mode.target);
    let steps: BulkStep[];
    let describe = (done: readonly BulkStep[]) => bulkText("edit", done);
    switch (mode.prompt) {
      case "title":
      case "meeting":
        return;
      case "labels": {
        const edits = parseLabelEdits(mode.line.text);
        steps = stepsOf(items, (i) => {
          const labels = applyLabelEdits(i.labels, edits);
          return sameLabels(labels, i.labels) ? undefined : updateStep({ id: i.id, labels }, ["labels"]);
        });
        if (!steps.length) return this.done("Labels unchanged");
        break;
      }
      case "due":
      case "defer": {
        const due = mode.prompt === "due";
        const text = mode.line.text.trim();
        let date: ProtoDate | undefined;
        if (text) {
          date = await this.parse(mode, undefined, text);
          if (!date) return;
        }
        steps = stepsOf(items, (i) =>
          sameDate(date, due ? i.dueDate : i.deferUntil)
            ? undefined
            : updateStep(due ? { id: i.id, dueDate: date } : { id: i.id, deferUntil: date }, [due ? "due_date" : "defer_until"]),
        );
        if (!steps.length) return this.done(due ? "Due dates unchanged" : "Defer dates unchanged");
        break;
      }
      case "waiting": {
        const w = await this.bulkWaiting(mode, items);
        if (!w) return;
        steps = w;
        if (!steps.length) return this.done("Waiting unchanged");
        break;
      }
      case "repeat": {
        const r = await this.bulkRepeat(mode, items);
        if (!r) return;
        steps = r;
        if (!steps.length) return this.done("Repeat rules unchanged");
        const verb = mode.line.text.trim() ? "set repeat" : "stop repeating";
        describe = (done) => bulkText(verb, done, true);
        break;
      }
    }
    if (await this.bulk.run(steps, describe)) this.c.setMode(normalMode);
  }

  /** Closes the prompt, saying why nothing was sent. */
  private done(text: string): void {
    this.c.setMode(normalMode);
    this.c.toast("info", text);
  }

  /**
   * w with marks, per Item as for one: the same person changes only the
   * follow-up, another person replaces waiting, and empty clears it.
   * Undefined when the prompt stays open.
   */
  private async bulkWaiting(mode: Prompt, items: readonly Item[]): Promise<BulkStep[] | undefined> {
    const { person, date: dateText } = parseWaiting(mode.line.text);
    if (!person) {
      if (dateText !== undefined) {
        this.refuse(mode, "Who is it waiting on? Type a name before the ^date");
        return undefined;
      }
      return stepsOf(items, (i) => (i.waiting ? updateStep({ id: i.id }, ["waiting"]) : undefined));
    }
    let followUpOn: ProtoDate | undefined;
    if (dateText !== undefined) {
      followUpOn = await this.parse(mode, undefined, dateText);
      if (!followUpOn) return undefined;
    }
    return stepsOf(items, (i) => {
      const current = i.waiting;
      if (current && waitingOn(i) === person) {
        if (sameDate(followUpOn, current.followUpOn)) return undefined;
        const waiting = { on: current.on, since: current.since, reason: current.reason, followUpOn };
        return updateStep({ id: i.id, waiting }, ["waiting.follow_up_on"]);
      }
      const waiting = { on: { ref: { case: "freeText" as const, value: person } }, ...(followUpOn && { followUpOn }) };
      return updateStep({ id: i.id, waiting }, ["waiting"]);
    });
  }

  /**
   * @ with marks, per Item as for one: the text is resolved against each
   * Item's own due date (one ParseRecurrence per date they have), so
   * `weekly` repeats each on its own weekday; empty stops every repeat.
   * Undefined when the prompt stays open.
   */
  private async bulkRepeat(mode: Prompt, items: readonly Item[]): Promise<BulkStep[] | undefined> {
    const text = mode.line.text.trim();
    const step = (id: string, rule: RecurrenceRule | undefined) => ({ kind: { case: "setRecurrence" as const, value: { id, rule } } });
    if (!text) return stepsOf(items, (i) => (i.recurrence ? step(i.id, undefined) : undefined));
    if (this.anchorMoved(mode, this.repeatQuery(mode).key)) return undefined;
    const today = this.c.today();
    const rules = new Map<string, RecurrenceRule>();
    for (const item of items) {
      const key = anchorKey(item, today);
      if (rules.has(key)) continue;
      const rule = await this.parseRepeat(mode, undefined, text, anchorOf(item, text, today));
      if (!rule) return undefined;
      rules.set(key, rule);
    }
    return stepsOf(items, (i) => {
      const rule = rules.get(anchorKey(i, today));
      return sameRule(rule, i.recurrence?.rule) ? undefined : step(i.id, rule);
    });
  }

  /** ParseRecurrence on enter (the preview may be stale): the rule, or the prompt kept open with the warning. */
  private async parseRepeat(mode: Prompt, item: Item | undefined, text: string, anchor: ProtoDate | undefined): Promise<RecurrenceRule | undefined> {
    let res: ParseRecurrenceResponse;
    try {
      res = await this.c.pipeline.call(({ timeoutMs }) =>
        parseRecurrence(this.c.items, text, { ...(timeoutMs !== undefined && { timeoutMs }), ...(anchor && { anchor }) }),
      );
    } catch (err) {
      this.c.failed(errorText(err, item));
      return undefined;
    }
    if (res.rule && !res.warning) return res.rule;
    const latest = this.latest(mode);
    this.c.setMode({ ...latest, preview: { for: latest.line.text, text: repeatPreview(res, this.c.today()), warn: true } });
    this.c.dropQueued();
    return undefined;
  }

  /** ParseDate on enter (the preview may be stale): the date, or the prompt kept open with the warning. */
  private async parse(mode: Prompt, item: Item | undefined, text: string): Promise<ProtoDate | undefined> {
    let res: ParseDateResponse;
    try {
      res = await this.c.pipeline.call(({ timeoutMs }) => parseDate(this.c.items, text, { ...(timeoutMs !== undefined && { timeoutMs }) }));
    } catch (err) {
      this.c.failed(errorText(err, item));
      return undefined;
    }
    if (res.date) return res.date;
    const latest = this.latest(mode);
    this.c.setMode({ ...latest, preview: { for: latest.line.text, text: singleLine(res.warning), warn: true } });
    this.c.dropQueued();
    return undefined;
  }
}
