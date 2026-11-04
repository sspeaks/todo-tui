import { useState } from "react";
import { LabelsSheet } from "../pickers/LabelsSheet";
import { Pill } from "./Pill";

/** The labels in use, each one tap on and off; "New label" for another. */
export function QuickLabels({
  value,
  known,
  onChange,
  limit,
}: {
  value: readonly string[];
  /** Most used first. */
  known: readonly string[];
  onChange: (labels: string[]) => void;
  /** Show at most this many of the unchosen ones; the sheet has the rest. */
  limit?: number;
}) {
  const [adding, setAdding] = useState(false);
  const unchosen = known.filter((l) => !value.includes(l));
  const shown = limit === undefined ? unchosen : unchosen.slice(0, limit);
  const all = [...value, ...shown];
  const more = unchosen.length > shown.length;
  const toggle = (l: string) => onChange(value.includes(l) ? value.filter((x) => x !== l) : [...value, l]);
  return (
    <>
      {all.map((l) => (
        <Pill key={l} label={`#${l}`} on={value.includes(l)} onPress={() => toggle(l)} />
      ))}
      <Pill icon="add" label={more ? "More…" : "New label"} onPress={() => setAdding(true)} accessibilityLabel={more ? "More labels" : "New label"} />
      {adding ? <LabelsSheet visible value={value} known={known} onChange={onChange} onClose={() => setAdding(false)} /> : null}
    </>
  );
}
