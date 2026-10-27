// The text the t and w prompts edit, both ways: labels as "#a #b", and
// waiting as "<person> [^date]" (Decision 5).

/** "#a #b": the prefill of `t`. */
export const formatLabels = (labels: readonly string[]): string => labels.map((l) => `#${l}`).join(" ");

/** The labels typed into `t`: words, each without its leading #s, the first of each kept. */
export function parseLabels(text: string): string[] {
  const out: string[] = [];
  for (const word of text.split(/\s+/)) {
    const label = word.replace(/^#+/, "");
    if (label && !out.includes(label)) out.push(label);
  }
  return out;
}

/** Whether two label lists hold the same set. */
export const sameLabels = (a: readonly string[], b: readonly string[]): boolean => a.length === b.length && a.every((l) => b.includes(l));

/** What `w` was given: the person, and the follow-up date text after a trailing ^ (with the ^). */
export interface WaitingText {
  readonly person: string;
  readonly date?: string;
}

/** "dana ^fri" is dana, follow up ^fri; the person may hold spaces; only a last word that starts with ^ is a date. */
export function parseWaiting(text: string): WaitingText {
  const m = /(?:^|\s)(\^\S*)\s*$/u.exec(text);
  if (!m?.[1]) return { person: text.trim() };
  return { person: text.slice(0, m.index).trim(), date: m[1] };
}

/** The person part of `w`'s text, up to a ^date, for completion. */
export const waitingPrefix = (text: string): string => {
  const at = text.search(/(?:^|\s)\^/u);
  return at < 0 ? text : text.slice(0, at);
};

/** The longest prefix `names` share, compared without case. */
export function commonPrefix(names: readonly string[]): string {
  const [first, ...rest] = names;
  if (first === undefined) return "";
  let cps = [...first];
  for (const n of rest) {
    const other = [...n.toLowerCase()];
    let i = 0;
    while (i < cps.length && cps[i]?.toLowerCase() === other[i]) i++;
    cps = cps.slice(0, i);
  }
  return cps.join("");
}
